//! Reading pacman's on-disk state: sync databases, the local database, and
//! the `.PKGINFO` inside a package file.
//!
//! This is the whole data layer — no `pacman -Ss`/`-Qi` subprocesses. The
//! formats are alpm's own, stable for years: sync DBs are compressed tar
//! archives of `<pkgname>/desc` files, the local DB is a directory of the same
//! `desc` files (with `%REASON%` and `%DEPENDS%`), and `.PKGINFO` is a
//! `key = value` text file at the root of the package archive.

use std::fs;
use std::io::{Cursor, Read};
use std::path::Path;

/// One `%KEY%` block of a desc file: the key and its value lines.
pub type Sections = Vec<(String, Vec<String>)>;

/// Parse the alpm desc format: `%KEY%` on its own line, value lines until
/// the next key. Blank lines separate blocks and are not values.
pub fn parse_desc(text: &str) -> Sections {
    let mut sections: Sections = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with('%') && line.ends_with('%') && line.len() > 2 {
            sections.push((line.to_string(), Vec::new()));
        } else if let Some((_, values)) = sections.last_mut() && !line.is_empty() {
            values.push(line.to_string());
        }
    }
    sections
}

/// The first value of a section, if it has one.
pub fn field<'a>(sections: &'a Sections, key: &str) -> Option<&'a str> {
    sections
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.first())
        .map(String::as_str)
}

/// All values of a section (empty if the key is absent).
pub fn multi(sections: &Sections, key: &str) -> Vec<String> {
    sections
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
}

fn field_u64(sections: &Sections, key: &str) -> u64 {
    field(sections, key).and_then(|v| v.parse().ok()).unwrap_or(0)
}

/// A package entry from a sync repository database.
#[derive(Clone, Debug)]
pub struct SyncPackage {
    pub name: String,
    pub version: String,
    pub desc: String,
    pub url: String,
    pub repository: String,
    pub csize: u64,
    pub isize: u64,
    pub depends: Vec<String>,
    /// Names this package answers for besides its own (`p7zip` is how 7zip
    /// appears to a dependency). A dependency matched only by name would ask
    /// for a package that does not exist.
    pub provides: Vec<String>,
}

/// Unpack a sync DB into the tar stream inside it.
///
/// The local file keeps the compression the mirror served: Arch's
/// `core.db.tar.gz` lands as `/var/lib/pacman/sync/core.db`, so the official
/// databases are **gzip**, not zstd (`repo-add` writes gzip too; zstd appears
/// when a repo is published that way). libalpm sniffs the format, and so must
/// we — assuming one codec leaves every repository unreadable, which looks
/// exactly like a system with nothing to update.
fn unpack_db(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    if bytes.starts_with(b"\x1f\x8b") {
        flate2::read::GzDecoder::new(bytes).read_to_end(&mut out)?;
        Ok(out)
    } else if bytes.starts_with(b"\x28\xb5\x2f") {
        zstd::stream::decode_all(bytes)
    } else if bytes.starts_with(b"\xfd7zXZ\x00") {
        Err(unsupported("xz"))
    } else if bytes.starts_with(b"BZh") {
        Err(unsupported("bzip2"))
    } else {
        Err(unsupported("an unknown format"))
    }
}

fn unsupported(format: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("package database is compressed with {format}, which this app cannot read"),
    )
}

/// Read a sync DB (`/var/lib/pacman/sync/<repo>.db`) into entries.
pub fn parse_sync_db(bytes: &[u8], repository: &str) -> std::io::Result<Vec<SyncPackage>> {
    let data = unpack_db(bytes)?;
    let mut archive = tar::Archive::new(Cursor::new(data));
    let mut out = Vec::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        // Entries are `<pkgname>/desc` (plus files/mtree we ignore).
        if path.file_name().map(|n| n != "desc").unwrap_or(true) {
            continue;
        }
        let mut text = String::new();
        entry.read_to_string(&mut text)?;
        let s = parse_desc(&text);
        let Some(name) = field(&s, "%NAME%").map(str::to_string) else {
            continue;
        };
        out.push(SyncPackage {
            name,
            version: field(&s, "%VERSION%").unwrap_or("").to_string(),
            desc: field(&s, "%DESC%").unwrap_or("").to_string(),
            url: field(&s, "%URL%").unwrap_or("").to_string(),
            repository: repository.to_string(),
            csize: field_u64(&s, "%CSIZE%"),
            isize: field_u64(&s, "%ISIZE%"),
            depends: multi(&s, "%DEPENDS%"),
            provides: multi(&s, "%PROVIDES%"),
        });
    }
    Ok(out)
}

/// How deep `Include` nesting is followed before the list is called
/// unenumerable. Stock configs nest once (`/etc/pacman.d/mirrorlist`).
const MAX_CONFIG_INCLUDE_DEPTH: usize = 16;

/// The repositories a pacman.conf declares, in the order it declares them.
///
/// Which `.db` files in the sync directory are channels is the configuration's
/// decision, not the directory's. Measured with pacman 7 against two throwaway
/// repositories carrying the same package name: a database on disk whose
/// repository the config does not name is invisible (`-Sl` with `zz-first.db`
/// in `sync/` and only `zz-second` configured lists only `zz-second`), and
/// when two configured repositories carry one name the candidate for an install
/// or an upgrade is the copy in the **first** section — `-Sp` and `-Qu` flip
/// when the two sections are swapped in the file.
///
/// `None` means the list could not be enumerated: an unreadable file, an
/// `Include` with glob(7) wildcards, or an include cycle. A caller must treat
/// that as "do not filter" — dropping repositories that were never seen is how
/// a package list reads as empty by accident.
pub fn read_config_repos(path: &Path) -> Option<Vec<String>> {
    let text = fs::read_to_string(path).ok()?;
    let mut repos = Vec::new();
    let base = path.parent().unwrap_or(Path::new("/"));
    collect_config_repos(&text, base, 0, &mut repos)?;
    Some(repos)
}

/// The config format: comments start a line, a section header sits alone on
/// its line, and `Include` works in `[options]` and in a repository section
/// and may itself declare repositories — so it is followed where it appears,
/// which is the top-to-bottom order pacman processes the file in. `[options]`
/// is not a repository and `local` is reserved for the installed package DB;
/// repository names must be unique, so a repeated section is one channel.
fn collect_config_repos(
    text: &str,
    base: &Path,
    depth: usize,
    repos: &mut Vec<String>,
) -> Option<()> {
    for line in text.lines() {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let name = line[1..line.len() - 1].trim();
            let known = repos.iter().any(|r| r == name);
            if !name.is_empty() && name != "options" && name != "local" && !known {
                repos.push(name.to_string());
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("include") {
            continue;
        }
        let value = value.trim();
        if value.contains(['*', '?', '[']) {
            // A glob(7) include: not worth a glob implementation, and a wrong
            // guess would hide a repository.
            return None;
        }
        let path = if value.starts_with('/') {
            std::path::PathBuf::from(value)
        } else {
            base.join(value)
        };
        // A missing include is a warning in pacman, not a reason to stop.
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        if depth >= MAX_CONFIG_INCLUDE_DEPTH {
            return None; // an include cycle: the list has no end
        }
        let base = path.parent().unwrap_or(Path::new("/"));
        collect_config_repos(&text, base, depth + 1, repos)?;
    }
    Some(())
}

/// pacman's install reason — exactly the `-Qe` distinction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// Chosen by the user (`pacman -S`).
    Explicit,
    /// Pulled in as a dependency.
    Dependency,
}

/// Read a `%REASON%` value with alpm's semantics, measured against pacman
/// 7.1 on a real system: pacman writes the section **only for dependencies**,
/// as the numeric alpm enum (1 = `PKG_INSTALLED`, 2 = `PKG_EXPLICIT`); older
/// pacman wrote the words `Installed`/`Explicit`. A missing section — every
/// explicit package on a current pacman — means explicit. An unparseable
/// value falls to dependency: the fail-closed choice, keeping the package
/// off the Installed page rather than offering to remove it.
pub fn parse_reason(value: Option<&str>) -> Reason {
    match value.map(str::trim) {
        None | Some("Explicit") | Some("2") => Reason::Explicit,
        _ => Reason::Dependency,
    }
}

/// A package installed on this system.
#[derive(Clone, Debug)]
pub struct LocalPackage {
    pub name: String,
    pub version: String,
    pub desc: String,
    pub reason: Reason,
    pub depends: Vec<String>,
    /// The same `%PROVIDES%` section the sync databases carry: alpm writes it
    /// into the local DB too, so a dependency answered by an installed
    /// package under another name is still satisfied.
    pub provides: Vec<String>,
}

/// Read the local DB (`/var/lib/pacman/local/*/desc`).
pub fn read_local_db(dir: &Path) -> std::io::Result<Vec<LocalPackage>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let desc_path = entry?.path().join("desc");
        let Ok(text) = fs::read_to_string(&desc_path) else {
            continue; // a partial install dir: not a package we can show
        };
        let s = parse_desc(&text);
        let Some(name) = field(&s, "%NAME%").map(str::to_string) else {
            continue;
        };
        out.push(LocalPackage {
            name,
            version: field(&s, "%VERSION%").unwrap_or("").to_string(),
            desc: field(&s, "%DESC%").unwrap_or("").to_string(),
            reason: parse_reason(field(&s, "%REASON%")),
            depends: multi(&s, "%DEPENDS%"),
            provides: multi(&s, "%PROVIDES%"),
        });
    }
    Ok(out)
}

/// Metadata from the `.PKGINFO` of a local package file (the file-open flow).
#[derive(Clone, Debug)]
pub struct PkgInfo {
    pub name: String,
    pub version: String,
    pub desc: String,
    pub url: String,
    pub isize: u64,
    pub depends: Vec<String>,
}

/// Stream a `.pkg.tar.zst` just far enough to read its `.PKGINFO`. Streaming
/// matters: these files are routinely hundreds of MB, and `.PKGINFO` is the
/// first member.
pub fn read_pkginfo(path: &Path) -> std::io::Result<PkgInfo> {
    let file = fs::File::open(path)?;
    let decoder = zstd::stream::read::Decoder::new(file)?;
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.path()? != Path::new(".PKGINFO") {
            continue;
        }
        let mut text = String::new();
        entry.read_to_string(&mut text)?;
        return parse_pkginfo(&text).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, ".PKGINFO has no pkgname")
        });
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no .PKGINFO in the archive",
    ))
}

/// Parse the `key = value` form of `.PKGINFO` (repeated keys collect).
pub fn parse_pkginfo(text: &str) -> Option<PkgInfo> {
    let mut get: Vec<(&str, String)> = Vec::new();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            get.push((k.trim(), v.trim().to_string()));
        }
    }
    let one = |k: &str| {
        get.iter()
            .find(|(key, _)| *key == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let name = one("pkgname");
    if name.is_empty() {
        return None;
    }
    Some(PkgInfo {
        name,
        version: one("pkgver"),
        desc: one("pkgdesc"),
        url: one("url"),
        isize: one("size").parse().unwrap_or(0),
        depends: get
            .iter()
            .filter(|(k, _)| *k == "depend")
            .map(|(_, v)| v.clone())
            .collect(),
    })
}

/// The leading package name of a dependency string (`glibc>=2.0` → `glibc`).
pub fn dep_name(dep: &str) -> &str {
    let bytes = dep.as_bytes();
    let end = bytes
        .iter()
        .position(|c| !(c.is_ascii_alphanumeric() || matches!(c, b'@' | b'.' | b'_' | b'+' | b'-')))
        .unwrap_or(bytes.len());
    &dep[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desc(pairs: &[(&str, &[&str])]) -> String {
        let mut s = String::new();
        for (k, vs) in pairs {
            s.push_str(&format!("%{k}%\n"));
            for v in vs.iter() {
                s.push_str(v);
                s.push('\n');
            }
            s.push('\n');
        }
        s
    }

    #[test]
    fn desc_round_trip() {
        let text = desc(&[
            ("NAME", &["hello"]),
            ("VERSION", &["2.12.1-1"]),
            ("DESC", &["example package"]),
            ("DEPENDS", &["glibc", "foo>=1.0"]),
            ("PROVIDES", &["bar"]),
        ]);
        let s = parse_desc(&text);
        assert_eq!(field(&s, "%NAME%"), Some("hello"));
        assert_eq!(field(&s, "%DESC%"), Some("example package"));
        assert_eq!(multi(&s, "%DEPENDS%"), ["glibc", "foo>=1.0"]);
        assert_eq!(multi(&s, "%PROVIDES%"), ["bar"]);
        assert_eq!(field_u64(&s, "%ISIZE%"), 0);
    }

    /// The tar half of a sync DB: two `desc` members, uncompressed.
    fn sync_db_tar() -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut ar = tar::Builder::new(&mut buf);
            for (name, desc_text) in [
                ("hello", desc(&[("NAME", &["hello"]), ("VERSION", &["2.12.1-1"]), ("DESC", &["example"]), ("CSIZE", &["1234"]), ("ISIZE", &["5678"])])),
                ("world", desc(&[("NAME", &["world"]), ("VERSION", &["1.0-1"]), ("PROVIDES", &["p7zip", "libp7zip.so=1-64"])])),
            ] {
                let mut header = tar::Header::new_ustar();
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(desc_text.len() as u64);
                header.set_mode(0o644);
                header.set_mtime(0);
                ar.append_data(&mut header, format!("{name}/desc"), desc_text.as_bytes())
                    .unwrap();
            }
            ar.finish().unwrap();
        }
        buf
    }

    fn gz(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = flate2::write::GzEncoder::new(&mut out, flate2::Compression::default());
        std::io::Write::write_all(&mut enc, bytes).unwrap();
        enc.finish().unwrap();
        out
    }

    fn zst(bytes: &[u8]) -> Vec<u8> {
        zstd::stream::encode_all(bytes, 1).unwrap()
    }

    #[test]
    fn sync_db_fixture() {
        let pkgs = parse_sync_db(&zst(&sync_db_tar()), "extra").unwrap();
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[0].name, "hello");
        assert_eq!(pkgs[0].repository, "extra");
        assert_eq!(pkgs[0].csize, 1234);
        assert_eq!(pkgs[1].name, "world");
        // Measured against extra.db: 7zip's desc carries `Provides: p7zip`.
        assert_eq!(pkgs[1].provides, ["p7zip", "libp7zip.so=1-64"]);
    }

    #[test]
    fn sync_db_reads_the_compression_the_mirror_sent() {
        // The format Arch actually ships: `core.db` is `core.db.tar.gz`. A
        // zstd-only reader parses none of the official repositories and the
        // Updates page then says "Everything up to date".
        for (label, bytes) in [("gzip", gz(&sync_db_tar())), ("zstd", zst(&sync_db_tar()))] {
            let pkgs = parse_sync_db(&bytes, "core").unwrap_or_else(|e| panic!("{label}: {e}"));
            assert_eq!(pkgs.len(), 2, "{label}");
            assert_eq!(pkgs[0].name, "hello", "{label}");
            assert_eq!(pkgs[0].repository, "core", "{label}");
            assert_eq!(pkgs[0].csize, 1234, "{label}");
        }
    }

    #[test]
    fn sync_db_names_a_codec_it_cannot_read() {
        // Not silently empty: the message is what the user is shown.
        let e = parse_sync_db(b"\xfd7zXZ\x00\x00\x00", "core").unwrap_err();
        assert!(e.to_string().contains("xz"), "{e}");
        let e = parse_sync_db(b"not an archive", "core").unwrap_err();
        assert!(e.to_string().contains("unknown"), "{e}");
    }

    /// A scratch directory for config fixtures. Named per test and per process
    /// so tests running in parallel cannot collide.
    fn fixture_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("aelum-pkg-installer-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A pacman.conf whose `Include` files cannot be read, so the test sees
    /// only the section rules.
    fn config_repos(text: &str) -> Option<Vec<String>> {
        let mut repos = Vec::new();
        collect_config_repos(text, Path::new("/aelum-test-missing"), 0, &mut repos)?;
        Some(repos)
    }

    #[test]
    fn config_repositories_are_listed_in_the_order_declared() {
        let text = "\
# a pacman.conf
[options]
SigLevel = Required DatabaseOptional
Include = /etc/pacman.d/mirrorlist

[core]
Server = https://example.com/core

# a commented-out repository stays out
# [testing]
[extra]
Server = https://example.com/extra
# a repeated section is one channel, and `local` is the installed DB
[core]
Server = https://mirror/alt
[local]
";
        assert_eq!(config_repos(text).unwrap(), ["core", "extra"]);
    }

    #[test]
    fn config_includes_are_followed_where_they_appear() {
        let dir = fixture_dir("config_include");
        let included = dir.join("extra.list");
        fs::write(&included, "[extra]\nServer = file:///extra\n").unwrap();
        let text = format!("[options]\nInclude = {}\n[core]\nServer = file:///core\n", included.display());
        let mut repos = Vec::new();
        collect_config_repos(&text, dir.as_path(), 0, &mut repos).unwrap();
        // The included repository comes before the one declared after the
        // include: that is the order pacman resolves a name in.
        assert_eq!(repos, ["extra", "core"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_config_whose_repositories_cannot_be_enumerated_says_so() {
        // glob(7) includes are documented in pacman.conf(5); guessing at them
        // could hide a repository, so the caller is told not to filter.
        assert_eq!(config_repos("[options]\nInclude = /etc/pacman.d/*.conf\n"), None);
        // …and a config that simply has no repositories is an empty list, not
        // an unknown one: there is genuinely nothing to install from.
        assert_eq!(config_repos("[options]\nSigLevel = Never\n").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn pkginfo_fixture() {
        let text = "\
# generated by makepkg
pkgname = aelum-demo
pkgver = 0.2.0-1
pkgdesc = A demo package
url = https://aelum.dev
size = 4096
packager = Aelum <aelum@aelum.dev>
license = GPL3
depend = glibc
depend = gtk4>=4.24
";
        let info = parse_pkginfo(text).unwrap();
        assert_eq!(info.name, "aelum-demo");
        assert_eq!(info.version, "0.2.0-1");
        assert_eq!(info.isize, 4096);
        assert_eq!(info.depends, ["glibc", "gtk4>=4.24"]);
    }

    #[test]
    fn dep_names_strip_comparators() {
        assert_eq!(dep_name("glibc"), "glibc");
        assert_eq!(dep_name("gtk4>=4.24"), "gtk4");
        assert_eq!(dep_name("python>=3.11"), "python");
        assert_eq!(dep_name("lib32-gcc-libs"), "lib32-gcc-libs");
    }

    #[test]
    fn reason_matches_alpm_semantics() {
        // Measured against pacman 7.1: the section is written only for
        // dependencies (glibc → `1`), and explicit packages (gimp, steam)
        // have no section at all.
        assert_eq!(parse_reason(None), Reason::Explicit);
        assert_eq!(parse_reason(Some("1")), Reason::Dependency);
        assert_eq!(parse_reason(Some("2")), Reason::Explicit);
        // The pre-numeric format stays readable.
        assert_eq!(parse_reason(Some("Installed")), Reason::Dependency);
        assert_eq!(parse_reason(Some("Explicit")), Reason::Explicit);
        // Unknown values fail closed: a package we cannot classify is not
        // offered for removal.
        assert_eq!(parse_reason(Some("whatever")), Reason::Dependency);
    }
}
