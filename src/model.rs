//! The merged package model: sync entries × local state, classification,
//! and search. Pure — built by loading, read by the UI, never mutates disk.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use crate::db::{self, LocalPackage, Reason, SyncPackage};
use crate::vercmp::vercmp;

/// What a row can do with a package.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Not installed; can be installed.
    Available,
    /// Installed explicitly (`-Qe`); can be removed.
    InstalledExplicit,
    /// Installed as a dependency; not offered for removal.
    InstalledDep,
    /// Installed, and a sync repo carries a newer version.
    UpdateAvailable,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub description: String,
    /// The sync repository that carries it (`Some`), or `None` when the
    /// package is installed but in no configured repo (foreign, orphaned).
    pub repository: Option<String>,
    pub installed: Option<LocalPackage>,
    pub sync: Option<SyncPackage>,
}

impl Entry {
    pub fn status(&self) -> Status {
        match (&self.installed, &self.sync) {
            (Some(local), Some(sync)) => {
                if vercmp(&sync.version, &local.version) == std::cmp::Ordering::Greater {
                    Status::UpdateAvailable
                } else if local.reason == Reason::Explicit {
                    Status::InstalledExplicit
                } else {
                    Status::InstalledDep
                }
            }
            (Some(local), None) => {
                if local.reason == Reason::Explicit {
                    Status::InstalledExplicit
                } else {
                    Status::InstalledDep
                }
            }
            (None, _) => Status::Available,
        }
    }

    /// The version to show: the sync one when available (it is what an
    /// install would bring), else the installed one.
    pub fn display_version(&self) -> &str {
        self.sync
            .as_ref()
            .map(|s| s.version.as_str())
            .or_else(|| self.installed.as_ref().map(|l| l.version.as_str()))
            .unwrap_or("")
    }
}

/// This app's own pacman package. It is hidden from every list (Installed,
/// Updates, Browse) — a package manager does not manage itself — but stays in
/// the model so removing a package it depends on still reports the breakage.
/// Must match `pkgname` in packaging/PKGBUILD.
pub const SELF_PACKAGE: &str = "aelum-package-installer";

/// Which rows a search may return.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Explicitly installed packages — the Installed page.
    Installed,
    /// Everything: sync repos plus installed-only leftovers.
    All,
}

pub struct Model {
    entries: Vec<Entry>,
    index: HashMap<String, usize>,
    /// Newest mtime among the sync DBs: how fresh the browse page is.
    pub db_fresh: Option<SystemTime>,
}

impl Model {
    /// Load from a pacman root (normally `/var/lib/pacman`).
    pub fn load(pacman_root: &Path) -> std::io::Result<Model> {
        let local = db::read_local_db(&pacman_root.join("local"))?;

        let mut sync = Vec::new();
        let mut db_fresh = None;
        let sync_dir = pacman_root.join("sync");
        if let Ok(rd) = fs::read_dir(&sync_dir) {
            for f in rd.flatten() {
                let path = f.path();
                let Some(repo) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let Some(repo) = repo.strip_suffix(".db") else { continue };
                let bytes = fs::read(&path).unwrap_or_default();
                if let Ok(pkgs) = db::parse_sync_db(&bytes, repo) {
                    sync.extend(pkgs);
                }
                if let Ok(meta) = f.metadata()
                    && let Ok(t) = meta.modified()
                {
                    db_fresh = Some(db_fresh.map_or(t, |cur: SystemTime| cur.max(t)));
                }
            }
        }

        let mut index: HashMap<String, usize> = HashMap::new();
        let mut entries: Vec<Entry> = Vec::new();
        for p in sync {
            index.insert(p.name.clone(), entries.len());
            entries.push(Entry {
                description: p.desc.clone(),
                repository: Some(p.repository.clone()),
                installed: None,
                name: p.name.clone(),
                sync: Some(p),
            });
        }
        for l in local {
            match index.get(&l.name) {
                Some(&i) => entries[i].installed = Some(l),
                None => {
                    index.insert(l.name.clone(), entries.len());
                    let name = l.name.clone();
                    let description = l.desc.clone();
                    entries.push(Entry {
                        name,
                        description,
                        repository: None,
                        installed: Some(l),
                        sync: None,
                    });
                }
            }
        }

        Ok(Model { entries, index, db_fresh })
    }

    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.index.get(name).map(|&i| &self.entries[i])
    }

    /// Case-insensitive search ranked name-prefix > name-substring >
    /// description-substring, then by name.
    pub fn search(&self, query: &str, scope: Scope) -> Vec<&Entry> {
        let q = query.trim().to_lowercase();
        let mut hits: Vec<(u8, &Entry)> = Vec::new();
        for e in &self.entries {
            if e.name == SELF_PACKAGE {
                continue;
            }
            if scope == Scope::Installed
                && !matches!(e.status(), Status::InstalledExplicit | Status::UpdateAvailable)
            {
                continue;
            }
            let rank = if q.is_empty() {
                0
            } else {
                let name = e.name.to_lowercase();
                if name.starts_with(&q) {
                    0
                } else if name.contains(&q) {
                    1
                } else if e.description.to_lowercase().contains(&q) {
                    2
                } else {
                    continue;
                }
            };
            hits.push((rank, e));
        }
        hits.sort_by_key(|(rank, e)| (*rank, e.name.clone()));
        hits.into_iter().map(|(_, e)| e).collect()
    }

    pub fn updatable(&self) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|e| e.name != SELF_PACKAGE && e.status() == Status::UpdateAvailable)
            .collect()
    }

    /// Installed packages that depend on `name` — what removing it would
    /// break. The name is matched as the leading token of a dep string.
    pub fn reverse_deps(&self, name: &str) -> Vec<String> {
        self.entries
            .iter()
            .filter_map(|e| e.installed.as_ref())
            .filter(|l| {
                l.name != name
                    && l.depends.iter().any(|d| db::dep_name(d) == name)
            })
            .map(|l| l.name.clone())
            .collect()
    }

    /// Orphaned dependencies an uninstall with `-Rns` would take along:
    /// dependencies that no installed package outside the removal set
    /// depends on. Fixed-point: each new orphan can make more orphans.
    pub fn orphans_after_removal(&self, name: &str) -> Vec<String> {
        let mut removed: Vec<String> = vec![name.to_string()];
        let mut changed = true;
        while changed {
            changed = false;
            for e in &self.entries {
                let Some(l) = e.installed.as_ref() else { continue };
                if l.reason == Reason::Explicit || removed.contains(&l.name) {
                    continue;
                }
                // Still needed only if some installed package outside the
                // removal set depends on it. Dependents, not dependencies.
                let needed = self
                    .entries
                    .iter()
                    .filter_map(|e| e.installed.as_ref())
                    .any(|d| {
                        !removed.contains(&d.name)
                            && d.depends.iter().any(|dep| db::dep_name(dep) == l.name)
                    });
                if !needed {
                    removed.push(l.name.clone());
                    changed = true;
                }
            }
        }
        removed.remove(0); // the package being removed itself
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::SyncPackage;

    fn sync(name: &str, version: &str, desc: &str) -> SyncPackage {
        SyncPackage {
            name: name.into(),
            version: version.into(),
            desc: desc.into(),
            url: String::new(),
            repository: "extra".into(),
            csize: 0,
            isize: 0,
            depends: vec![],
        }
    }

    fn local(name: &str, version: &str, reason: Reason, depends: &[&str]) -> LocalPackage {
        LocalPackage {
            name: name.into(),
            version: version.into(),
            desc: String::new(),
            reason,
            isize: 0,
            depends: depends.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn model(entries: Vec<Entry>) -> Model {
        let mut index = HashMap::new();
        for (i, e) in entries.iter().enumerate() {
            index.insert(e.name.clone(), i);
        }
        Model { entries, index, db_fresh: None }
    }

    fn entry(name: &str, installed: Option<LocalPackage>, sync_pkg: Option<SyncPackage>) -> Entry {
        Entry {
            name: name.into(),
            description: sync_pkg.as_ref().map(|s| s.desc.clone()).unwrap_or_default(),
            repository: sync_pkg.as_ref().map(|s| s.repository.clone()),
            installed,
            sync: sync_pkg,
        }
    }

    fn fixture() -> Model {
        model(vec![
            entry("firefox", None, Some(sync("firefox", "1.0", "browser"))),
            entry("htop", Some(local("htop", "3.0-1", Reason::Explicit, &[])), Some(sync("htop", "3.1-1", "monitor"))),
            entry("glibc", Some(local("glibc", "2.40-1", Reason::Dependency, &[])), Some(sync("glibc", "2.40-1", "libc"))),
            entry("myapp", Some(local("myapp", "1.0-1", Reason::Explicit, &["glibc"])), None),
            entry("orphan-dep", Some(local("orphan-dep", "1.0-1", Reason::Dependency, &["myapp"])), None),
        ])
    }

    #[test]
    fn status_classification() {
        let m = fixture();
        assert_eq!(m.get("firefox").unwrap().status(), Status::Available);
        assert_eq!(m.get("htop").unwrap().status(), Status::UpdateAvailable);
        assert_eq!(m.get("glibc").unwrap().status(), Status::InstalledDep);
        assert_eq!(m.get("myapp").unwrap().status(), Status::InstalledExplicit);
    }

    #[test]
    fn installed_scope_lists_only_explicit_and_updatable() {
        let m = fixture();
        let names: Vec<_> = m.search("", Scope::Installed).iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["htop", "myapp"]);
        // The dep and the not-installed entry stay out of the page.
        assert_eq!(m.get("orphan-dep").unwrap().status(), Status::InstalledDep);
        assert_eq!(m.get("firefox").unwrap().status(), Status::Available);
    }

    #[test]
    fn search_ranks_name_before_description() {
        let m = fixture();
        let hits = m.search("brow", Scope::All);
        assert_eq!(hits.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["firefox"]);
        let hits = m.search("mon", Scope::All);
        assert_eq!(hits[0].name, "htop");
    }

    #[test]
    fn updatable_lists_newer_sync_versions_only() {
        let m = fixture();
        assert_eq!(m.updatable().iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["htop"]);
    }

    #[test]
    fn self_package_is_excluded_from_every_list() {
        // The app's own package: hidden from Installed, Browse and Updates,
        // yet still a real dependent in the model.
        let m = model(vec![
            entry(
                SELF_PACKAGE,
                Some(local(SELF_PACKAGE, "0.1.0-1", Reason::Explicit, &["polkit"])),
                Some(sync(SELF_PACKAGE, "0.2.0-1", "the package manager")),
            ),
            entry("htop", Some(local("htop", "3.0-1", Reason::Explicit, &[])), Some(sync("htop", "3.1-1", "monitor"))),
            entry("polkit", Some(local("polkit", "128-1", Reason::Dependency, &[])), None),
        ]);
        // Not in any list, even when searched by name or a name prefix.
        for scope in [Scope::Installed, Scope::All] {
            assert!(!m.search("", scope).iter().any(|e| e.name == SELF_PACKAGE));
            assert!(!m.search("aelum", scope).iter().any(|e| e.name == SELF_PACKAGE));
        }
        // It would otherwise be updatable (0.1.0-1 < 0.2.0-1): still hidden.
        assert_eq!(m.updatable().iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["htop"]);
        // Still a real dependent: removing polkit would break it.
        assert_eq!(m.reverse_deps("polkit"), [SELF_PACKAGE]);
    }

    #[test]
    fn reverse_deps_match_the_dep_token() {
        let m = fixture();
        assert_eq!(m.reverse_deps("glibc"), ["myapp"]);
        assert!(m.reverse_deps("firefox").is_empty());
    }

    #[test]
    fn orphans_follow_the_removal() {
        let m = fixture();
        // glibc's only dependent (myapp) goes, and orphan-dep has no
        // dependents at all: `-Rns myapp` would take both.
        assert_eq!(m.orphans_after_removal("myapp"), ["glibc", "orphan-dep"]);
        // Removing glibc leaves myapp (explicit) standing; only the
        // never-needed orphan falls.
        assert_eq!(m.orphans_after_removal("glibc"), ["orphan-dep"]);
    }

    /// The classification must agree with pacman's own `-Qe` on a real
    /// system. Ignored by default: it needs an Arch host and shells out
    /// to pacman. Run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn explicit_matches_pacman_qe() {
        let m = Model::load(Path::new("/var/lib/pacman")).expect("real pacman root");
        // Classify by reason, not Status: `-Qe` also lists packages that
        // have an update pending.
        let ours: Vec<String> = m
            .search("", Scope::All)
            .iter()
            .filter(|e| {
                e.installed.as_ref().is_some_and(|l| l.reason == Reason::Explicit)
            })
            .map(|e| e.name.clone())
            .collect();
        let out = std::process::Command::new("pacman")
            .args(["-Qqe"])
            .output()
            .expect("pacman");
        let mut theirs: Vec<String> = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            // search() hides the app's own package, so drop it from the
            // pacman side too — the comparison is about classifying the
            // others, and the self-exclusion has its own test.
            .filter(|l| *l != SELF_PACKAGE)
            .map(str::to_string)
            .collect();
        theirs.sort();
        let mut ours = ours;
        ours.sort();
        assert_eq!(ours, theirs);
    }
}
