//! Reading pacman's on-disk state: sync databases, the local database, and
//! the `.PKGINFO` inside a package file.
//!
//! This is the whole data layer — no `pacman -Ss`/`-Qi` subprocesses. The
//! formats are alpm's own, stable for years: sync DBs are tar.zst archives
//! of `<pkgname>/desc` files, the local DB is a directory of the same
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
}

/// Read a sync DB (`/var/lib/pacman/sync/<repo>.db`) into entries.
pub fn parse_sync_db(bytes: &[u8], repository: &str) -> std::io::Result<Vec<SyncPackage>> {
    let data = zstd::stream::decode_all(bytes)?;
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
        });
    }
    Ok(out)
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
    pub isize: u64,
    pub depends: Vec<String>,
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
            isize: field_u64(&s, "%ISIZE%"),
            depends: multi(&s, "%DEPENDS%"),
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
        ]);
        let s = parse_desc(&text);
        assert_eq!(field(&s, "%NAME%"), Some("hello"));
        assert_eq!(field(&s, "%DESC%"), Some("example package"));
        assert_eq!(multi(&s, "%DEPENDS%"), ["glibc", "foo>=1.0"]);
        assert_eq!(field_u64(&s, "%ISIZE%"), 0);
    }

    #[test]
    fn sync_db_fixture() {
        // Build a real tar.zst in memory, then parse it back.
        let mut buf = Vec::new();
        {
            let enc = zstd::stream::write::Encoder::new(&mut buf, 1).unwrap();
            let mut ar = tar::Builder::new(enc);
            for (name, desc_text) in [
                ("hello", desc(&[("NAME", &["hello"]), ("VERSION", &["2.12.1-1"]), ("DESC", &["example"]), ("CSIZE", &["1234"]), ("ISIZE", &["5678"])])),
                ("world", desc(&[("NAME", &["world"]), ("VERSION", &["1.0-1"])])),
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
            ar.into_inner().unwrap().finish().unwrap();
        }
        let pkgs = parse_sync_db(&buf, "extra").unwrap();
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[0].name, "hello");
        assert_eq!(pkgs[0].repository, "extra");
        assert_eq!(pkgs[0].csize, 1234);
        assert_eq!(pkgs[1].name, "world");
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
