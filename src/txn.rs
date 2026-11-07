//! Package transactions: the one place this app asks for root.
//!
//! Every privileged action runs through the package's own helper under
//! pkexec — the polkit action's `exec.path` annotation authenticates the
//! *program*, exactly like `aelum-snapshot-restore`. The helper is the only
//! place pacman is invoked with privileges, so one action + one rule cover
//! install, removal, refresh, upgrade, and local files.
//!
//! `--noconfirm` everywhere: this dialog owns the confirmation, and a
//! conflict must exit non-zero with pacman's real output rather than wait
//! for a TTY that does not exist.

use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;

use futures::channel::mpsc::UnboundedSender;

use crate::i18n;
use crate::progress;

/// The helper shipped by the package; pkexec resolves it to the
/// `dev.aelum.package.manage` action through the policy's exec.path.
pub const HELPER: &str = "/usr/bin/aelum-package-installer-helper";

#[derive(Clone, Debug)]
pub enum Txn {
    Refresh,
    Install(Vec<String>),
    /// The AUR plan's repository half — installed **as dependencies**, the
    /// marking makepkg's `-s` uses (`run_pacman -S --asdeps`). A name
    /// installed by name is Explicit: it survives every `-Rns` and the next
    /// plan reads it as already satisfied, so a build dep left behind by a
    /// failed cleanup would be a permanent ghost.
    InstallAsDeps(Vec<String>),
    Remove(Vec<String>),
    Upgrade,
    /// Local package files — an opened `.pkg.tar.zst`, or the requested
    /// package's own files. Several files are one transaction: pacman resolves the
    /// order between them, which is what a split PKGBUILD needs.
    InstallFiles(Vec<PathBuf>),
    /// The built *dependencies'* archives — the same marking as
    /// `InstallAsDeps`. The requested package stays Explicit; a dependency
    /// that outlives the cleanup should be an orphan, not a ghost.
    InstallFilesAsDeps(Vec<PathBuf>),
}

impl Txn {
    /// The helper's argv (without the `pkexec` itself) — separate from the
    /// spawn so it can be tested without root.
    pub fn argv(&self) -> Vec<String> {
        match self {
            Txn::Refresh => vec!["refresh".into()],
            Txn::Install(pkgs) => {
                let mut a = vec!["install".into()];
                a.extend(pkgs.iter().cloned());
                a
            }
            Txn::InstallAsDeps(pkgs) => {
                let mut a = vec!["install-asdeps".into()];
                a.extend(pkgs.iter().cloned());
                a
            }
            Txn::Remove(pkgs) => {
                let mut a = vec!["remove".into()];
                a.extend(pkgs.iter().cloned());
                a
            }
            Txn::Upgrade => vec!["upgrade".into()],
            Txn::InstallFiles(paths) | Txn::InstallFilesAsDeps(paths) => {
                let verb = match self {
                    Txn::InstallFilesAsDeps(_) => "file-asdeps",
                    _ => "file",
                };
                let mut a = vec![verb.into()];
                a.extend(paths.iter().map(|p| p.display().to_string()));
                a
            }
        }
    }

    /// One line shown in the log while it runs.
    pub fn summary(&self) -> String {
        match self {
            Txn::Refresh => i18n::t("txn.summaryRefresh"),
            Txn::Install(pkgs) | Txn::InstallAsDeps(pkgs) => {
                i18n::t_fmt("txn.summaryInstall", &[("names", &pkgs.join(", "))])
            }
            Txn::Remove(pkgs) => i18n::t_fmt("txn.summaryRemove", &[("names", &pkgs.join(", "))]),
            Txn::Upgrade => i18n::t("txn.summaryUpgrade"),
            Txn::InstallFiles(paths) | Txn::InstallFilesAsDeps(paths) => i18n::t_fmt(
                "txn.summaryFile",
                &[(
                    "file",
                    &paths
                        .iter()
                        .map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join(", "),
                )],
            ),
        }
    }

    /// Which progress bands this request runs through (`progress::Track`).
    /// The AUR job is not a `Txn` — its steps are announced, not parsed —
    /// and builds its track as `Verb::Aur` directly.
    pub fn verb(&self) -> crate::progress::Verb {
        match self {
            Txn::Refresh => crate::progress::Verb::Refresh,
            Txn::Install(_) | Txn::InstallAsDeps(_) => crate::progress::Verb::Install,
            Txn::Remove(_) => crate::progress::Verb::Remove,
            Txn::Upgrade => crate::progress::Verb::Upgrade,
            Txn::InstallFiles(_) | Txn::InstallFilesAsDeps(_) => crate::progress::Verb::File,
        }
    }

    /// Whether this verb fetches the sync databases — the helper runs `-Sy`
    /// for `refresh` and `-Syu` for `upgrade`. Only those two leave the
    /// databases current, so only their success may mark a check as done;
    /// every other verb reads the databases and proves nothing about their age.
    pub fn syncs_databases(&self) -> bool {
        matches!(self, Txn::Refresh | Txn::Upgrade)
    }
}

/// Progress of a running transaction, streamed to the UI.
#[derive(Clone, Debug)]
pub enum TxnEvent {
    Line(String),
    /// A named step of a multi-step job (the AUR flow) began. The progress
    /// bar follows these for that flow — driving it by scraping translated
    /// log text is exactly what breaks in another language.
    Step(progress::StepKind),
    /// Ok(0) on success; Err(code) for anything else (pkexec's 127 means
    /// the action was refused, 1 means the user dismissed the agent).
    Finished(Result<(), i32>),
}

/// Start the transaction. Output lines and the exit status arrive on `tx`;
/// the child is reaped by the reader threads.
///
/// The helper path is not optional and there is no raw-`pacman` fallback:
/// every privileged operation must resolve to the one polkit action,
/// exactly like `aelum-snapshot-restore`. A host without the packaged
/// helper cannot authorize any transaction, so the caller checks the path
/// first and says so plainly.
pub fn run(txn: &Txn, tx: UnboundedSender<TxnEvent>) -> std::io::Result<()> {
    let mut child = spawn(txn)?;
    read_pipes(&mut child, &tx);
    // The caller is not held: the exit status arrives as an event.
    thread::spawn(move || {
        let status = child.wait().map(|s| s.code().unwrap_or(-1));
        let _ = tx.unbounded_send(match status {
            Ok(0) => TxnEvent::Finished(Ok(())),
            Ok(code) => TxnEvent::Finished(Err(code)),
            Err(_) => TxnEvent::Finished(Err(-1)),
        });
    });
    Ok(())
}

/// Several transactions as one job: every step runs in order on a worker
/// (stopping at the first failure), all output streams into the same
/// channel, and the exit status of the job is the failing step's. This is
/// what lets a chooser-answered request install its providers `--asdeps`
/// before the names the user asked for, in one log dialog.
pub fn run_all(steps: &[Txn], tx: UnboundedSender<TxnEvent>) -> std::io::Result<()> {
    if let [only] = steps {
        return run(only, tx);
    }
    let steps = steps.to_vec();
    thread::spawn(move || {
        let mut outcome = Ok(());
        for step in &steps {
            if let Err(code) = run_wait(step, &tx) {
                outcome = Err(code);
                break;
            }
        }
        let _ = tx.unbounded_send(TxnEvent::Finished(outcome));
    });
    Ok(())
}

/// The same transaction, run to completion on the calling thread, with its
/// output on `tx` and its exit code as the return value.
///
/// This is what lets one user request be several steps: an AUR install brings
/// its repository dependencies in through here before it builds, and all of
/// it streams into the one log dialog the request already opened.
pub fn run_wait(txn: &Txn, tx: &UnboundedSender<TxnEvent>) -> Result<(), i32> {
    let mut child = match spawn(txn) {
        Ok(child) => child,
        Err(e) => {
            let _ = tx.unbounded_send(TxnEvent::Line(e.to_string()));
            return Err(-1);
        }
    };
    read_pipes(&mut child, tx);
    match child.wait() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(status.code().unwrap_or(-1)),
        Err(_) => Err(-1),
    }
}

/// The helper, under pkexec, with both pipes piped and not yet read.
fn spawn(txn: &Txn) -> std::io::Result<std::process::Child> {
    Command::new("pkexec")
        .arg(HELPER)
        .args(txn.argv())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

/// Both pipes get their own reader: leaving one unread can deadlock a long
/// pacman run on a full pipe buffer.
fn read_pipes(child: &mut std::process::Child, tx: &UnboundedSender<TxnEvent>) {
    if let Some(out) = child.stdout.take() {
        pipe(out, tx);
    }
    if let Some(err) = child.stderr.take() {
        pipe(err, tx);
    }
}

fn pipe<R: std::io::Read + Send + 'static>(reader: R, tx: &UnboundedSender<TxnEvent>) {
    let tx = tx.clone();
    thread::spawn(move || {
        for line in std::io::BufReader::new(reader).lines().map_while(Result::ok) {
            let _ = tx.unbounded_send(TxnEvent::Line(line));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The helper's whole interface, in its own order. The tests that read
    /// the helper script ask about exactly these, so a verb added to the
    /// helper cannot be missed by one of them.
    const VERBS: [&str; 7] = [
        "refresh",
        "install",
        "install-asdeps",
        "remove",
        "upgrade",
        "file",
        "file-asdeps",
    ];

    #[test]
    fn argv_matches_the_helper_subcommands() {
        assert_eq!(Txn::Refresh.argv(), ["refresh"]);
        assert_eq!(Txn::Install(vec!["htop".into()]).argv(), ["install", "htop"]);
        assert_eq!(
            Txn::InstallAsDeps(vec!["cmake".into()]).argv(),
            ["install-asdeps", "cmake"]
        );
        assert_eq!(
            Txn::Remove(vec!["a".into(), "b".into()]).argv(),
            ["remove", "a", "b"]
        );
        assert_eq!(Txn::Upgrade.argv(), ["upgrade"]);
        assert_eq!(
            Txn::InstallFiles(vec!["/home/u/x.pkg.tar.zst".into()]).argv(),
            ["file", "/home/u/x.pkg.tar.zst"]
        );
        // A split PKGBUILD: several files, one pacman call, in build order.
        assert_eq!(
            Txn::InstallFiles(vec!["/t/a.pkg.tar.zst".into(), "/t/b.pkg.tar.zst".into()]).argv(),
            ["file", "/t/a.pkg.tar.zst", "/t/b.pkg.tar.zst"]
        );
        // Built dependencies' archives carry makepkg's marking.
        assert_eq!(
            Txn::InstallFilesAsDeps(vec!["/t/helper.pkg.tar.zst".into()]).argv(),
            ["file-asdeps", "/t/helper.pkg.tar.zst"]
        );
    }

    #[test]
    fn only_the_verbs_that_fetch_databases_mark_them_current() {
        // The freshness record the next launch reads is only honest when a
        // call that really re-fetched the databases wrote it: the helper's
        // `-Sy` and `-Syu`. Every other verb reads those databases, so its
        // success says nothing about how old they are.
        let helper = include_str!("../data/helper/aelum-package-installer-helper");
        assert!(
            helper.contains("pacman -Sy --noconfirm"),
            "refresh must fetch the databases"
        );
        assert!(
            helper.contains("pacman -Syu --noconfirm"),
            "upgrade must fetch the databases"
        );
        for txn in [Txn::Refresh, Txn::Upgrade] {
            assert!(txn.syncs_databases(), "{txn:?} fetches the databases");
        }
        for txn in [
            Txn::Install(vec!["htop".into()]),
            Txn::InstallAsDeps(vec!["cmake".into()]),
            Txn::Remove(vec!["htop".into()]),
            Txn::InstallFiles(vec!["/t/x.pkg.tar.zst".into()]),
            Txn::InstallFilesAsDeps(vec!["/t/x.pkg.tar.zst".into()]),
        ] {
            assert!(!txn.syncs_databases(), "{txn:?} only reads the databases");
        }
    }

    #[test]
    fn the_helper_is_annotated_by_the_policy() {
        // The pattern that makes pkexec resolve `pkexec <helper>` to the
        // dev.aelum.package.manage action — the aelum-snapshot-restore
        // shape: the policy names the exact program, and it is the same
        // path this file execs.
        let policy = include_str!("../data/polkit/dev.aelum.package.manage.policy");
        assert!(policy.contains(&format!("exec.path\">{HELPER}<")));
        let helper = include_str!("../data/helper/aelum-package-installer-helper");
        for verb in VERBS {
            assert!(helper.contains(&format!("{verb})")), "helper lacks {verb}");
            assert!(helper.contains("--noconfirm"));
        }
        // Line-buffered: with stdout a pipe glibc holds pacman's output in
        // 4 KiB blocks, and the app would watch a frozen log through a
        // whole download (measured: 53 s of silence for ~200 MiB).
        assert!(helper.contains("stdbuf -oL"), "helper must line-buffer");
    }

    #[test]
    fn every_verb_clears_a_stale_db_lock_but_never_a_live_one() {
        let helper = include_str!("../data/helper/aelum-package-installer-helper");
        // The lock lives in the DB root this app reads its databases from:
        // one path, not a second copy of it.
        assert!(
            helper.contains(&format!("DB_LOCK={}/db.lck", crate::model::PACMAN_ROOT)),
            "the lock must be the one in the DB root the model reads"
        );
        // One clear, for every verb, before any pacman runs. alpm never drops
        // a leftover db.lck itself — measured on pacman 7.1: an empty O_EXCL
        // file, no PID written, and no dead-lock check anywhere — so the lock
        // a killed transaction leaves behind is every later transaction's
        // "unable to lock database", whichever verb it asked for.
        let gated = VERBS.join("|");
        assert!(
            helper.contains(&format!("    {gated}) clear_stale_lock ;;")),
            "every verb must clear the lock"
        );
        assert!(
            helper.find("clear_stale_lock ;;").unwrap_or(usize::MAX)
                < helper.find("exec stdbuf -oL pacman").unwrap_or(0),
            "the lock must be cleared before pacman runs"
        );
        // A live pacman is a transaction in progress: dropping its lock would
        // let two of them write the same databases.
        assert!(helper.contains("pacman_running"), "must keep a live lock");
    }
}
