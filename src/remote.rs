//! The outside world: the commands this app runs **without** root.
//!
//! `aur` describes the AUR's protocol and `txn` runs everything that needs
//! privileges; this file is the part in between — asking the RPC a question,
//! downloading the PKGBUILD the AUR advertises, and building it. Those are
//! the only subprocesses in the program that are not root: an AUR package is
//! built by the user who asked for it, in their own cache directory, and only
//! the finished `.pkg.tar.zst` goes to the helper and `pacman -U`.
//!
//! curl rather than an HTTP client crate: `pacman` depends on curl (measured
//! with `pacman -Qi pacman`: `Depends On: … curl …`), so it is present on
//! every system this app runs on, and no new library enters the build image.

use std::collections::{BTreeMap, HashSet};
use std::io::BufRead;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use futures::channel::mpsc::UnboundedSender;

use crate::aur;
use crate::db;
use crate::i18n;
use crate::model::{self, Model};
use crate::txn::{self, Txn, TxnEvent};

/// How many rounds of "what does that need?" one install may ask. A
/// PKGBUILD can name anything, so the walk is bounded; names it never got an
/// answer for are reported, not guessed at.
const MAX_INFO_ROUNDS: usize = 4;
/// …and how many names in total.
const MAX_QUERIES: usize = 100;

/// The RPC reply for a URL, as text.
pub fn http_get(url: &str) -> Result<String, String> {
    let bytes = http_bytes(url)?;
    String::from_utf8(bytes).map_err(|_| "the AUR answered with something that is not text".into())
}

/// A URL's body. `--fail` makes an HTTP error a non-zero exit rather than a
/// body full of HTML, which would otherwise reach the parser as nonsense.
pub fn http_bytes(url: &str) -> Result<Vec<u8>, String> {
    let out = Command::new("curl")
        .args(["-sS", "--fail", "--location", "--max-time", "120"])
        .arg(url)
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("could not run curl: {e}"))?;
    if out.status.success() {
        return Ok(out.stdout);
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(if err.is_empty() {
        format!("the request failed (exit {})", out.status.code().unwrap_or(-1))
    } else {
        err
    })
}

/// Where AUR build directories live. `$XDG_CACHE_HOME` first, like every
/// other cache on the desktop; the build is not a download the user should
/// have to find again, so it lives and dies under the app's own name.
pub fn build_root() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".cache"))
        })
        .ok_or_else(|| "no cache directory to build in".to_string())?;
    Ok(base.join("aelum").join("package-installer").join("aur"))
}

/// A private, empty directory for one package base. Always fresh: a leftover
/// tree from an older build is the classic way a PKGBUILD build succeeds with
/// the previous version's sources still in it.
pub fn prepare_dir(name: &str) -> Result<PathBuf, String> {
    let dir = build_root()?.join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("could not clear {}: {e}", dir.display()))?;
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// Unpack a PKGBUILD snapshot into `dir`.
///
/// The AUR's snapshots are `<pkgbase>/PKGBUILD` plus the files it references,
/// gzipped. The leading directory is dropped — `dir` *is* the package
/// directory. A member that would escape it (`..`, an absolute path) is
/// skipped rather than trusted.
pub fn unpack_snapshot(bytes: &[u8], dir: &Path) -> Result<(), String> {
    let tar = if bytes.starts_with(b"\x1f\x8b") {
        let mut out = Vec::new();
        use std::io::Read;
        flate2::read::GzDecoder::new(bytes)
            .read_to_end(&mut out)
            .map_err(|e| format!("the snapshot is not valid gzip: {e}"))?;
        out
    } else {
        return Err("the AUR snapshot is not a .tar.gz".into());
    };
    let mut archive = tar::Archive::new(std::io::Cursor::new(tar));
    for entry in archive.entries().map_err(|e| format!("the snapshot is not a tar: {e}"))? {
        let mut entry = entry.map_err(|e| format!("the snapshot is damaged: {e}"))?;
        let path = entry
            .path()
            .map_err(|_| "the snapshot has a name this system cannot use".to_string())?
            .to_path_buf();
        // Drop the leading `<pkgbase>/` and anything that would escape dir.
        let mut parts = path.components();
        if parts.next().is_none() {
            continue;
        }
        let rest: PathBuf = parts.collect();
        if rest.as_os_str().is_empty()
            || rest
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::RootDir))
        {
            continue;
        }
        let target = dir.join(&rest);
        if entry.header().entry_type().is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let mut file = std::fs::File::create(&target)
            .map_err(|e| format!("could not write {}: {e}", target.display()))?;
        std::io::copy(&mut entry, &mut file)
            .map_err(|e| format!("could not write {}: {e}", target.display()))?;
    }
    Ok(())
}

/// Build the PKGBUILD in `dir`, streaming makepkg's own output into `tx`, and
/// return the archives it produced.
///
/// No `-s`: installing anything needs root, and the dependencies are brought
/// through the one polkit action before this runs — a `makepkg -s` would ask
/// sudo for a terminal this process does not have. No `-i` either: the
/// finished files go to the helper with everything else from the same
/// request, in one `pacman -U`.
pub fn build(dir: &Path, tx: &UnboundedSender<TxnEvent>) -> Result<Vec<PathBuf>, String> {
    let code = stream(
        "makepkg",
        &["--noconfirm"],
        Some(dir),
        tx,
        "could not run makepkg",
    )?;
    if code != 0 {
        return Err(format!("makepkg exited with {code}"));
    }
    let files = built_archives(dir);
    if files.is_empty() {
        return Err("makepkg produced no package file".into());
    }
    Ok(files)
}

/// The `.pkg.tar.*` files a build left behind. A split PKGBUILD produces
/// several, and all of them belong to one install.
///
/// makepkg is asked rather than the directory read: `--packagelist` names the
/// files this PKGBUILD produces — every half of a split build, wherever
/// `PKGDEST` in the user's makepkg.conf sent them, with whatever `PKGEXT` they
/// carry. Reading the directory finds neither. The scan is the fallback for a
/// makepkg that answers nothing (measured: no PKGBUILD → exit 12, no output),
/// and a path that is not a file is dropped here, because `pacman -U` answers
/// "could not find or read package" for one and that is a worse message than
/// this one.
fn built_archives(dir: &Path) -> Vec<PathBuf> {
    let mut files = packagelist(dir);
    if files.is_empty() {
        files = std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| is_archive(p))
                    .collect()
            })
            .unwrap_or_default();
    }
    files.retain(|p| p.is_file());
    files.sort();
    files.dedup();
    files
}

/// A built package file, whatever `PKGEXT` the system builds with. A `.sig`
/// is a signature of one, not one.
fn is_archive(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    !name.ends_with(".sig") && (name.ends_with(".pkg.tar") || name.contains(".pkg.tar."))
}

/// `makepkg --packagelist`: the absolute paths makepkg would write, without
/// building anything (measured: it needs no dependencies and no sources).
fn packagelist(dir: &Path) -> Vec<PathBuf> {
    let out = Command::new("makepkg")
        .arg("--packagelist")
        .current_dir(dir)
        .output()
        .ok()
        .filter(|o| o.status.success());
    let Some(out) = out else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Remove what a finished build has no further use for — the downloaded
/// sources, `$srcdir`, `$pkgdir` — and keep the archives it produced.
///
/// The archives live **inside** the build directory. Clearing the tree with
/// them (which is what this did first) is why a successful qtengine build
/// ended in `error: '…/qtengine-0.2.4-1-x86_64.pkg.tar.zst': could not find
/// or read package`: the files were gone before pacman was asked to read them.
fn keep_archives(dir: &Path, keep: &[PathBuf]) {
    let kept: Vec<_> = keep.iter().filter_map(|p| p.file_name()).collect();
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if kept.iter().any(|name| path.file_name() == Some(*name)) {
            continue;
        }
        if path.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// One pipe of a running command, read line by line on its own thread.
fn stream_pipe<R: std::io::Read + Send + 'static>(pipe: R, tx: &UnboundedSender<TxnEvent>) {
    let tx = tx.clone();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(pipe).lines().map_while(Result::ok) {
            let _ = tx.unbounded_send(TxnEvent::Line(line));
        }
    });
}

/// Run a command with its output streamed line by line, and wait for it.
/// Both pipes are read on their own thread: a full pipe buffer stops makepkg
/// mid-build (the same reason `txn::run` does this).
fn stream(
    program: &str,
    args: &[&str],
    dir: Option<&Path>,
    tx: &UnboundedSender<TxnEvent>,
    why: &str,
) -> Result<i32, String> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(dir.unwrap_or_else(|| Path::new("/")))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{why}: {e}"))?;
    if let Some(out) = child.stdout.take() {
        stream_pipe(out, tx);
    }
    if let Some(err) = child.stderr.take() {
        stream_pipe(err, tx);
    }
    let status = child
        .wait()
        .map_err(|e| format!("{why}: {e}"))?
        .code()
        .unwrap_or(-1);
    Ok(status)
}

/// One AUR install, start to finish, on one thread.
///
/// The event stream is the one `txn::run` produces — lines, then a final
/// status — so the log dialog, the running gate and the model reload are the
/// same code path as a plain `pacman -S`. One request, one dialog, even
/// though it is several kinds of work.
pub fn aur_install(hit: &aur::Hit, tx: &UnboundedSender<TxnEvent>) {
    // The steps' result *is* the Finished event, so the caller's log dialog,
    // running gate and model reload are the ones every transaction uses.
    let _ = tx.unbounded_send(TxnEvent::Finished(aur_steps(hit, tx)));
}

/// The steps: plan, bring the repository half, build, install, and take back
/// what only the build needed. A command's own output is already in the log
/// by the time it fails, so a failure here adds a line only when the app is
/// the one that knows something the command did not say.
fn aur_steps(hit: &aur::Hit, tx: &UnboundedSender<TxnEvent>) -> Result<(), i32> {
    if !safe_name(&hit.name) {
        return fail(tx, &format!("{}: not a package name", hit.name));
    }
    // The system as it is now, read here: the plan has to match the machine
    // the build runs on, not the one that was on screen when the search ran.
    let model = match Model::load(Path::new(model::PACMAN_ROOT), Path::new(model::PACMAN_CONFIG)) {
        Ok(model) => model,
        Err(e) => return fail(tx, &e.to_string()),
    };

    // 1. What the AUR says this needs. A dependency's own dependencies are
    //    only visible once that dependency has been asked about, so this runs
    //    in rounds.
    let mut infos: BTreeMap<String, aur::Info> = BTreeMap::new();
    let mut asked: HashSet<String> = HashSet::new();
    let mut next = vec![hit.name.clone()];
    let mut plan = aur::Plan::default();
    for _ in 0..MAX_INFO_ROUNDS {
        next.retain(|n| safe_name(n) && !asked.contains(n));
        next.sort();
        next.dedup();
        next.truncate(MAX_QUERIES.saturating_sub(asked.len()));
        if next.is_empty() {
            break;
        }
        asked.extend(next.iter().cloned());
        let body = match http_get(&aur::info_url(&next)) {
            Ok(body) => body,
            Err(e) => return fail(tx, &e),
        };
        match aur::parse_reply(&body) {
            Ok(aur::Response::Info(list)) => {
                for info in list {
                    infos.entry(info.name.clone()).or_insert(info);
                }
            }
            Ok(aur::Response::Search(_)) => {
                return fail(tx, "the AUR answered a different question");
            }
            Err(e) => return fail(tx, &e),
        }
        plan = aur::plan(&hit.name, &infos, &|name| {
            if model.dep_satisfied(name) {
                aur::Need::Satisfied
            } else if model.in_repo(name) {
                aur::Need::Repository
            } else if infos.contains_key(name) {
                // The AUR knows it, so it is buildable — and it has to be
                // built before the package that needs it.
                aur::Need::Build
            } else {
                aur::Need::Unknown
            }
        });
        next = plan.unknown.clone();
    }
    if !plan.unknown.is_empty() {
        return fail(
            tx,
            &i18n::t_fmt("aur.unresolved", &[("names", &plan.unknown.join(", "))]),
        );
    }

    // 2. The repository half, in one transaction through the one polkit
    //    action, marked as deps — makepkg's own marking (`run_pacman -S
    //    --asdeps`), so a name left behind is an orphan pacman can see and
    //    never an Explicit ghost the next plan reads as satisfied. makepkg
    //    without -s refuses to build while its dependencies are missing,
    //    and a `makepkg -s` would ask sudo for a terminal this process
    //    does not have.
    if !plan.repository.is_empty()
        && let Err(code) = txn::run_wait(&Txn::InstallAsDeps(plan.repository.clone()), tx)
    {
        return Err(code);
    }

    // 3. Build, in the order the plan produced. The archives split in two
    //    halves of one request: what the build produced *as a dependency*
    //    and what the user asked for — step 4 marks them differently.
    let mut dep_files: Vec<PathBuf> = Vec::new();
    let mut requested_files: Vec<PathBuf> = Vec::new();
    // The names this request puts on the system: the repository half, and
    // the pkgnames of every built *dependency* — never the requested
    // package's own halves. Step 5 decides from these what was only needed
    // for the build.
    let mut brought: Vec<String> = plan.repository.clone();
    for name in &plan.build {
        // The snapshot URL every answer carries; the AUR's own layout is the
        //    fallback for an answer that left it out, and a wrong one fails
        //    with curl's 404 in the log rather than quietly building
        //    something else.
        let path = infos
            .get(name)
            .and_then(|i| Some(i.url_path.clone()).filter(|p| !p.is_empty()))
            .or_else(|| {
                // The row the user clicked carries a URLPath of its own.
                Some(hit.url_path.clone()).filter(|p| !p.is_empty() && *name == hit.name)
            })
            .unwrap_or_else(|| format!("/cgit/aur.git/snapshot/{name}.tar.gz"));
        let dir = match prepare_dir(name) {
            Ok(dir) => dir,
            Err(e) => return fail(tx, &e),
        };
        if let Err(e) = http_bytes(&aur::snapshot_url(&path)).and_then(|b| unpack_snapshot(&b, &dir))
        {
            return fail(tx, &e);
        }
        let files = match build(&dir, tx) {
            Ok(files) => files,
            Err(e) => {
                // The tree stays where a build failed: it holds the PKGBUILD
                //    and the sources, which is what a failed build is read
                //    with.
                let _ = tx.unbounded_send(TxnEvent::Line(i18n::t_fmt(
                    "aur.buildDir",
                    &[("path", &dir.display().to_string())],
                )));
                return fail(tx, &e);
            }
        };
        // The sources are of no use once the package exists, and pacman keeps
        //    no copy of a file it installs with -U — but the archives are in
        //    this directory, so they are what stays.
        keep_archives(&dir, &files);
        if *name != hit.name {
            // A split PKGBUILD's halves carry names of their own, so the
            //    archives answer for what was installed; the plan's name is
            //    the fallback for an archive that cannot be read.
            for file in &files {
                match db::read_pkginfo(file) {
                    Ok(info) => brought.push(info.name),
                    Err(_) => brought.push(name.clone()),
                }
            }
            dep_files.extend(files);
        } else {
            requested_files.extend(files);
        }
    }

    // 4. Everything this request produced: the built dependencies' archives
    //    first and as deps (makepkg's marking again), then the requested
    //    package itself, Explicit. A split PKGBUILD's packages stay in one
    //    pacman call: it resolves the order between them.
    if !dep_files.is_empty() {
        txn::run_wait(&Txn::InstallFilesAsDeps(dep_files), tx)?;
    }
    txn::run_wait(&Txn::InstallFiles(requested_files), tx)?;

    // 5. Take back what only the build needed. Which of the brought names
    //    those are is answered by the installed packages, not by the plan:
    //    makepkg writes a built package's runtime dependencies into its
    //    %DEPENDS% and its build-only ones nowhere, so a name no installed
    //    package outside the leftovers needs — by name or by provision, the
    //    rule -Rns itself resolves a set by — was needed for the build
    //    alone. The system is read again here for the same reason it was
    //    read at the start: the answer has to be about the machine as it is
    //    now.
    let after = match Model::load(Path::new(model::PACMAN_ROOT), Path::new(model::PACMAN_CONFIG)) {
        Ok(model) => model,
        Err(e) => {
            let _ = tx.unbounded_send(TxnEvent::Line(e.to_string()));
            let _ = tx.unbounded_send(TxnEvent::Line(i18n::t("aur.cleanupFailed")));
            return Ok(());
        }
    };
    let leftovers = aur::unneeded_build_deps(&hit.name, &brought, &|name| {
        // Not on the system: there is nothing here to remove. Otherwise
        //    the installed packages that need it, by name or provision —
        //    aur weighs them against the set.
        after
            .get(name)
            .and_then(|e| e.installed.as_ref())
            .map(|_| after.needed_by_names(name))
    });
    if leftovers.is_empty() {
        return Ok(());
    }
    let _ = tx.unbounded_send(TxnEvent::Line(i18n::t("aur.cleanupDeps")));
    if txn::run_wait(&Txn::Remove(leftovers), tx).is_err() {
        // pacman's own reason is already in the log; what this adds is
        //    that the install succeeded and only the cleanup did not.
        let _ = tx.unbounded_send(TxnEvent::Line(i18n::t("aur.cleanupFailed")));
    }
    Ok(())
}

/// One line the app is adding to the log, then the failure.
fn fail(tx: &UnboundedSender<TxnEvent>, msg: &str) -> Result<(), i32> {
    let _ = tx.unbounded_send(TxnEvent::Line(msg.to_string()));
    Err(-1)
}

/// A name becomes a directory, and it arrives over the network.
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '@'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gz(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = flate2::write::GzEncoder::new(&mut out, flate2::Compression::default());
        std::io::Write::write_all(&mut enc, bytes).unwrap();
        enc.finish().unwrap();
        out
    }

    /// The shape the AUR serves for `URLPath`: one top-level directory named
    /// after the package base.
    fn snapshot() -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut ar = tar::Builder::new(&mut buf);
            for (path, text) in [
                ("qtengine/PKGBUILD", "pkgname=qtengine\n"),
                ("qtengine/.SRCINFO", "pkgbase = qtengine\n"),
                ("qtengine/files/config.json", "{}\n"),
            ] {
                let mut header = tar::Header::new_ustar();
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(text.len() as u64);
                header.set_mode(0o644);
                header.set_mtime(0);
                ar.append_data(&mut header, path, text.as_bytes()).unwrap();
            }
            ar.finish().unwrap();
        }
        buf
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aelum-package-installer-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_snapshot_lands_in_the_build_directory_itself() {
        let dir = scratch("snapshot");
        unpack_snapshot(&gz(&snapshot()), &dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("PKGBUILD")).unwrap(), "pkgname=qtengine\n");
        assert!(dir.join(".SRCINFO").exists());
        assert!(dir.join("files/config.json").exists());
        // The tarball's own top-level directory is not duplicated inside.
        assert!(!dir.join("qtengine").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_snapshot_that_is_not_one_says_so() {
        let dir = scratch("snapshot_bad");
        let err = unpack_snapshot(b"not a tarball", &dir).unwrap_err();
        assert!(err.contains(".tar.gz"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn built_archives_are_the_files_a_build_produced() {
        // No PKGBUILD here, so makepkg has nothing to list (measured: exit
        // 12, no output) and the directory scan answers instead.
        let dir = scratch("archives");
        for name in [
            "qtengine-0.2.4-1-x86_64.pkg.tar.zst",
            "qtengine-debug-0.2.4-1-x86_64.pkg.tar.xz",
            "qtengine-0.2.4-1-x86_64.pkg.tar.zst.sig",
            "qtengine-0.2.4-1-x86_64.pkg",
        ] {
            std::fs::write(dir.join(name), "x").unwrap();
        }
        std::fs::create_dir(dir.join("pkg")).unwrap();
        let found = built_archives(&dir);
        assert_eq!(
            found.len(),
            2,
            "a split PKGBUILD's archives, any PKGEXT; no signature, no directory: {found:?}"
        );
        assert!(found[0].to_str().unwrap().contains("qtengine-0.2.4"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_finished_build_keeps_the_archives_it_made() {
        // The bug this pins: the build directory was deleted with the archives
        // still inside it, and pacman answered "could not find or read
        // package" for files that had just been built.
        let dir = scratch("keep");
        let archive = dir.join("qtengine-0.2.4-1-x86_64.pkg.tar.zst");
        std::fs::write(&archive, "zst").unwrap();
        std::fs::write(dir.join("PKGBUILD"), "pkgname=qtengine\n").unwrap();
        std::fs::create_dir_all(dir.join("src/qtengine")).unwrap();
        std::fs::write(dir.join("src/qtengine/main.cpp"), "int main(){}").unwrap();
        std::fs::create_dir_all(dir.join("pkg/qtengine/usr/bin")).unwrap();
        keep_archives(&dir, std::slice::from_ref(&archive));
        assert!(archive.is_file(), "pacman still has a file to read");
        assert!(!dir.join("PKGBUILD").exists());
        assert!(!dir.join("src").exists());
        assert!(!dir.join("pkg").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The RPC's format is a fact about someone else's server, so it gets
    /// re-verified against it. Ignored by default: it needs the network. Run
    /// with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn the_live_rpc_answers_the_format_the_parser_reads() {
        let body = http_get(&aur::search_url("qtengine")).expect("search");
        let hits = match aur::parse_reply(&body).expect("a search answer") {
            aur::Response::Search(hits) => hits,
            _ => panic!("the search answered with something else"),
        };
        let hit = hits
            .iter()
            .find(|h| h.name == "qtengine")
            .expect("qtengine is in the AUR");
        assert!(!hit.url_path.is_empty() && !hit.version.is_empty());

        // The same for the information request, which is what a plan is built
        // from, and for the snapshot the plan downloads.
        let body = http_get(&aur::info_url(std::slice::from_ref(&hit.name))).expect("info");
        let infos = match aur::parse_reply(&body).expect("an info answer") {
            aur::Response::Info(infos) => infos,
            _ => panic!("the info request answered with something else"),
        };
        assert_eq!(infos.len(), 1);
        assert!(!infos[0].depends.is_empty(), "a PKGBUILD always needs something");

        let dir = scratch("live_snapshot");
        let bytes = http_bytes(&aur::snapshot_url(&infos[0].url_path)).expect("snapshot");
        unpack_snapshot(&bytes, &dir).expect("unpacked");
        assert!(dir.join("PKGBUILD").exists(), "the PKGBUILD is what makepkg reads");

        // …and makepkg names the files a build of this PKGBUILD produces,
        // including both halves of a split build. That list, not a directory
        // scan, is what the install is given.
        let listed = packagelist(&dir);
        let names: Vec<&str> = listed
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
            .filter(|n| n.ends_with(".pkg.tar.zst"))
            .collect();
        assert_eq!(
            names,
            [
                "qtengine-0.2.4-1-x86_64.pkg.tar.zst",
                "qtengine-debug-0.2.4-1-x86_64.pkg.tar.zst"
            ],
            "a split PKGBUILD, in its own directory: {listed:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_name_becomes_a_directory_only_if_it_can_be_one() {
        assert!(safe_name("qtengine") && safe_name("dux-shell-bin") && safe_name("a+b.c"));
        assert!(!safe_name(""));
        assert!(!safe_name(".."));
        assert!(!safe_name("../escape"));
        assert!(!safe_name("a/b"));
    }

    #[test]
    fn the_build_root_is_the_app_s_own_cache_directory() {
        // Set for the test only; a real session gets $XDG_CACHE_HOME or
        // ~/.cache, and the app never writes outside this. (Edition 2024
        // makes this unsafe: no other thread in this test binary reads it.)
        unsafe { std::env::set_var("XDG_CACHE_HOME", "/tmp/aelum-test-cache") };
        assert_eq!(
            build_root().unwrap(),
            PathBuf::from("/tmp/aelum-test-cache/aelum/package-installer/aur")
        );
    }
}
