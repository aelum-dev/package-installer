//! The merged package model: sync entries × local state, classification,
//! and search. Pure — built by loading, read by the UI, never mutates disk.

use std::collections::{HashMap, HashSet, VecDeque};
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
/// Updates, Browse, Aelum) — a package manager does not manage itself — but
/// stays in the model so removing a package it depends on still reports the
/// breakage. Must match `pkgname` in packaging/PKGBUILD.
pub const SELF_PACKAGE: &str = "aelum-package-installer";

/// The distro's own repository, named by its sync DB (`aelum.db`). It is the
/// channel new apps reach installed systems through; the Aelum page lists it.
pub const DISTRO_REPO: &str = "aelum";

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
    /// name → the installed packages whose dependency list names it, each list
    /// sorted and deduplicated. Built once at load: answering "what depends on
    /// this" by scanning every installed package's dependency list is
    /// quadratic, measured at ~2 s per Remove click on a 1258-package system.
    dependents: HashMap<String, Vec<String>>,
    /// Newest mtime among the sync DBs: how fresh the browse page is.
    pub db_fresh: Option<SystemTime>,
    /// Sync databases that could not be read (`<repo>: reason`). A database
    /// that fails to parse leaves its repository invisible, which otherwise
    /// looks exactly like a system with nothing to update — the UI shows these.
    pub db_errors: Vec<String>,
}

impl Model {
    /// Load from a pacman root (normally `/var/lib/pacman`).
    pub fn load(pacman_root: &Path) -> std::io::Result<Model> {
        let local = db::read_local_db(&pacman_root.join("local"))?;

        let mut sync = Vec::new();
        let mut db_fresh = None;
        let mut db_errors: Vec<String> = Vec::new();
        let sync_dir = pacman_root.join("sync");
        if let Ok(rd) = fs::read_dir(&sync_dir) {
            for f in rd.flatten() {
                let path = f.path();
                let Some(repo) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let Some(repo) = repo.strip_suffix(".db") else { continue };
                let bytes = match fs::read(&path) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        db_errors.push(format!("{repo}: {e}"));
                        continue;
                    }
                };
                match db::parse_sync_db(&bytes, repo) {
                    Ok(pkgs) => sync.extend(pkgs),
                    Err(e) => db_errors.push(format!("{repo}: {e}")),
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

        let dependents = dependents_index(&entries);

        Ok(Model { entries, index, dependents, db_fresh, db_errors })
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

    /// The distro's apps this system does not have yet: uninstalled packages
    /// from the `[aelum]` repository. Publishing an app to the repo puts it
    /// here; an installed one belongs to the Installed page, and an installed
    /// one with an update pending to the Updates page. Sorted by name — the
    /// DB order is arbitrary and the list is short enough to read whole.
    pub fn distro_apps(&self) -> Vec<&Entry> {
        let mut hits: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| {
                e.name != SELF_PACKAGE
                    && e.installed.is_none()
                    && e.repository.as_deref() == Some(DISTRO_REPO)
            })
            .collect();
        hits.sort_by(|a, b| a.name.cmp(&b.name));
        hits
    }

    /// Whether the `[aelum]` database was read at all, installed or not.
    /// The page needs this to tell "there is nothing to add" from "the
    /// repository is not here" — an empty list must never read as a fact
    /// the model cannot support.
    pub fn has_distro_repo(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.repository.as_deref() == Some(DISTRO_REPO))
    }

    /// Installed packages that depend on `name` — what removing it would break.
    /// Sorted, so a confirmation lists them in a stable order.
    pub fn reverse_deps(&self, name: &str) -> Vec<String> {
        self.dependents.get(name).cloned().unwrap_or_default()
    }

    /// The dependencies `-Rns <name>` would take along: the packages in that
    /// dependency tree which nothing outside the removal needs — pacman's own
    /// rule, so it recurses into the target's dependencies and never sweeps up
    /// unrelated leftovers. Walked outwards from the removal: a dependency
    /// falls when its last remaining dependent falls with it.
    pub fn orphans_after_removal(&self, name: &str) -> Vec<String> {
        let mut removed: HashSet<&str> = HashSet::new();
        removed.insert(name);
        let mut fell: Vec<&str> = Vec::new();
        let mut queue: VecDeque<&str> = VecDeque::new();
        queue.push_back(name);
        while let Some(going) = queue.pop_front() {
            let Some(local) = self.installed(going) else { continue };
            for dep in &local.depends {
                let dep = db::dep_name(dep);
                if dep == going || removed.contains(dep) {
                    continue;
                }
                // Only an installed dependency can fall, and a package the user
                // chose is never taken by `-Rns`.
                let Some(dependency) = self.installed(dep) else { continue };
                if dependency.reason == Reason::Explicit {
                    continue;
                }
                // Still needed while any package outside the removal needs it.
                let needed = self
                    .dependents
                    .get(dep)
                    .is_some_and(|ds| ds.iter().any(|d| !removed.contains(d.as_str())));
                if needed {
                    continue;
                }
                removed.insert(dep);
                fell.push(dep);
                queue.push_back(dep);
            }
        }
        let mut fell: Vec<String> = fell.iter().map(|s| s.to_string()).collect();
        fell.sort();
        fell
    }

    /// The installed record for a name, if the system has one.
    fn installed(&self, name: &str) -> Option<&LocalPackage> {
        self.get(name).and_then(|e| e.installed.as_ref())
    }
}

/// name → the installed packages whose dependency list names it, each list
/// sorted and deduplicated. A dependency string is matched by its leading
/// token and a package that names itself is skipped, the same rule the
/// one-off scans used.
fn dependents_index(entries: &[Entry]) -> HashMap<String, Vec<String>> {
    let mut index: HashMap<String, Vec<String>> = HashMap::new();
    for e in entries {
        let Some(l) = e.installed.as_ref() else { continue };
        for dep in &l.depends {
            let dep = db::dep_name(dep);
            if dep == l.name {
                continue;
            }
            index.entry(dep.to_string()).or_default().push(l.name.clone());
        }
    }
    for list in index.values_mut() {
        list.sort();
        list.dedup();
    }
    index
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
            depends: depends.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// A sync entry from the distro's own repository.
    fn aelum(name: &str, version: &str, desc: &str) -> SyncPackage {
        SyncPackage {
            repository: DISTRO_REPO.into(),
            ..sync(name, version, desc)
        }
    }

    fn model(entries: Vec<Entry>) -> Model {
        let mut index = HashMap::new();
        for (i, e) in entries.iter().enumerate() {
            index.insert(e.name.clone(), i);
        }
        let dependents = dependents_index(&entries);
        Model { entries, index, dependents, db_fresh: None, db_errors: Vec::new() }
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
    fn distro_apps_are_the_uninstalled_distro_repo_packages() {
        let m = model(vec![
            // Published to the channel, not on the system yet: the page's whole point.
            entry("aelum-newapp", None, Some(aelum("aelum-newapp", "1.0-1", "a new app"))),
            // Installed, current or updatable: the Installed and Updates pages own those.
            entry("aelum-player", Some(local("aelum-player", "2.0-1", Reason::Explicit, &[])), Some(aelum("aelum-player", "2.0-1", "the player"))),
            entry("aelum-old", Some(local("aelum-old", "1.0-1", Reason::Explicit, &[])), Some(aelum("aelum-old", "2.0-1", "updatable"))),
            // Not from the distro repo, and never the app itself.
            entry("firefox", None, Some(sync("firefox", "1.0", "browser"))),
            entry(SELF_PACKAGE, None, Some(aelum(SELF_PACKAGE, "0.2.0-1", "the package manager"))),
        ]);
        let names: Vec<_> = m.distro_apps().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["aelum-newapp"]);
        assert!(m.has_distro_repo());
    }

    #[test]
    fn a_system_without_the_distro_repo_says_so() {
        // The page must be able to tell "nothing to add" from "the repo is
        // not here" — an empty list is not a fact about an unread repository.
        let m = fixture();
        assert!(m.distro_apps().is_empty());
        assert!(!m.has_distro_repo());
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
        // `-Rns myapp` takes glibc: nothing else needs it. It does not take
        // orphan-dep, which is not in myapp's dependency tree — it depends on
        // myapp, which is also why pacman refuses this removal (reverse_deps
        // is the list that says so).
        assert_eq!(m.orphans_after_removal("myapp"), ["glibc"]);
        // Removing glibc breaks myapp (explicit): nothing of glibc's falls.
        assert_eq!(m.orphans_after_removal("glibc"), Vec::<String>::new());
    }

    #[test]
    fn orphans_fall_transitively() {
        // top → a → b → c: removing top takes all three, each losing its only
        // remaining dependent in turn.
        let m = model(vec![
            entry("top", Some(local("top", "1-1", Reason::Explicit, &["a"])), None),
            entry("a", Some(local("a", "1-1", Reason::Dependency, &["b"])), None),
            entry("b", Some(local("b", "1-1", Reason::Dependency, &["c"])), None),
            entry("c", Some(local("c", "1-1", Reason::Dependency, &[])), None),
        ]);
        assert_eq!(m.orphans_after_removal("top"), ["a", "b", "c"]);
    }

    #[test]
    fn a_shared_dependency_stays() {
        // glibc is needed by another package the user chose: neither removal
        // takes it, and an explicit package is never taken at all.
        let m = model(vec![
            entry("one", Some(local("one", "1-1", Reason::Explicit, &["glibc"])), None),
            entry("two", Some(local("two", "1-1", Reason::Explicit, &["glibc"])), None),
            entry("helper", Some(local("helper", "1-1", Reason::Dependency, &["glibc"])), None),
            entry("glibc", Some(local("glibc", "1-1", Reason::Dependency, &[])), None),
        ]);
        assert_eq!(m.orphans_after_removal("one"), Vec::<String>::new());
        assert_eq!(m.orphans_after_removal("two"), Vec::<String>::new());
        assert_eq!(m.orphans_after_removal("helper"), Vec::<String>::new());
        assert_eq!(m.reverse_deps("glibc"), ["helper", "one", "two"]);
    }

    #[test]
    fn the_index_matches_a_plain_scan() {
        // The index is an optimisation only: for every installed package, the
        // dependents it lists are exactly what scanning the entries finds.
        let m = fixture();
        for target in m.entries.iter().filter(|e| e.installed.is_some()) {
            let mut scanned: Vec<String> = m
                .entries
                .iter()
                .filter_map(|e| e.installed.as_ref())
                .filter(|l| {
                    l.name != target.name
                        && l.depends.iter().any(|d| db::dep_name(d) == target.name)
                })
                .map(|l| l.name.clone())
                .collect();
            scanned.sort();
            scanned.dedup();
            let indexed = m.dependents.get(target.name.as_str()).cloned().unwrap_or_default();
            assert_eq!(indexed, scanned, "dependents of {}", target.name);
        }
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

    /// The update list must agree with pacman's own `-Qu`. Ignored by
    /// default: it needs an Arch host with sync databases and shells out to
    /// pacman. Run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn updates_match_pacman_qu() {
        let m = Model::load(Path::new("/var/lib/pacman")).expect("real pacman root");
        // The failure this guards was silent: every official `.db` is gzip, a
        // zstd-only reader parsed none of them, and the Updates page said
        // "Everything up to date" on a system with updates pending.
        assert!(m.db_errors.is_empty(), "unreadable databases: {:?}", m.db_errors);
        assert!(
            m.search("", Scope::All).iter().any(|e| e.sync.is_some()),
            "no sync packages were read at all"
        );

        let ours: Vec<String> = m.updatable().iter().map(|e| e.name.clone()).collect();
        let out = std::process::Command::new("pacman")
            .args(["-Quq"])
            .output()
            .expect("pacman");
        let mut theirs: Vec<String> = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            // The app hides its own package from the Updates page.
            .filter(|l| *l != SELF_PACKAGE)
            .map(str::to_string)
            .collect();
        theirs.sort();
        let mut ours = ours;
        ours.sort();
        assert_eq!(ours, theirs);
    }

    /// A press of Remove answers both removal questions on the UI thread
    /// before the confirmation appears. Scanning every installed package per
    /// question took ~2 s each on a 1258-package system, so the budget here is
    /// the difference between a dialog and a freeze. Ignored by default: it
    /// needs a real Arch host.
    #[test]
    #[ignore]
    fn removal_questions_answer_on_a_ui_thread_budget() {
        let m = Model::load(Path::new("/var/lib/pacman")).expect("real pacman root");
        let installed: Vec<String> = m
            .search("", Scope::All)
            .iter()
            .filter(|e| e.installed.is_some())
            .map(|e| e.name.clone())
            .collect();
        let started = std::time::Instant::now();
        for name in &installed {
            let _ = m.reverse_deps(name);
            let _ = m.orphans_after_removal(name);
        }
        let took = started.elapsed();
        assert!(
            took < std::time::Duration::from_secs(2),
            "{} removals answered in {took:?} — the dependents index is not in use",
            installed.len()
        );
    }
}
