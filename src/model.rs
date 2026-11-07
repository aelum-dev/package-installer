//! The merged package model: sync entries × local state, classification,
//! and search. Pure — built by loading, read by the UI, never mutates disk.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use crate::aur;
use crate::db::{self, LocalPackage, Reason, SyncPackage};
use crate::vercmp::vercmp;

/// Where the model lives on every Aelum system.
pub const PACMAN_ROOT: &str = "/var/lib/pacman";
/// The configuration that says which of its sync databases are repositories
/// this system can install from (`Model::load`).
pub const PACMAN_CONFIG: &str = "/etc/pacman.conf";

/// What a row can do with a package.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Not installed; can be installed.
    Available,
    /// Installed explicitly (`-Qe`); offered for removal unless it is the
    /// app's own package (`Entry::can_remove`).
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

    /// Whether this is the package that ships this app.
    pub fn is_self(&self) -> bool {
        self.name == SELF_PACKAGE
    }

    /// Whether a page may offer a working Remove for it. `pacman -Rns` would
    /// remove the app's own package happily enough — the running process keeps
    /// its deleted binary — but the system would be left with no package
    /// manager GUI, and nothing inside it to install one again. So the row is
    /// listed and updated like any other and only the removal is refused.
    pub fn can_remove(&self) -> bool {
        !self.is_self()
    }
}

/// This app's own pacman package. It is listed like any other — it needs its
/// updates too — and only its removal is refused (`Entry::can_remove`).
/// Must match `pkgname` in packaging/PKGBUILD.
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
    /// The names an installed package answers for under another name, and the
    /// same for the configured repositories. alpm resolves a dependency by
    /// provision as well as by name (measured against extra.db: 7zip carries
    /// `Provides = p7zip`), so a name-only check asks for packages that do
    /// not exist.
    provided_installed: HashSet<String>,
    provided_repo: HashSet<String>,
    /// The newest mtime among the databases the model actually reads: how
    /// fresh the browse page is. A database for a repository the system does
    /// not use is left out — its date says nothing about this system.
    pub db_fresh: Option<SystemTime>,
    /// Sync databases that could not be read (`<repo>: reason`). A database
    /// that fails to parse leaves its repository invisible, which otherwise
    /// looks exactly like a system with nothing to update — the UI shows these.
    pub db_errors: Vec<String>,
}

impl Model {
    /// Load from a pacman root (normally `/var/lib/pacman`) together with the
    /// pacman.conf that names the repositories it may use (normally
    /// `/etc/pacman.conf`).
    pub fn load(pacman_root: &Path, config: &Path) -> std::io::Result<Model> {
        let local = db::read_local_db(&pacman_root.join("local"))?;
        // The configuration decides which databases are channels at all; see
        // [`db::read_config_repos`]. `None` means it could not be read, and
        // then every database is kept — an unreadable config is not a reason
        // to show an empty list.
        let configured = db::read_config_repos(config);

        let mut by_repo: HashMap<String, Vec<SyncPackage>> = HashMap::new();
        let mut fresh: HashMap<String, SystemTime> = HashMap::new();
        let mut db_errors: Vec<String> = Vec::new();
        let sync_dir = pacman_root.join("sync");
        if let Ok(rd) = fs::read_dir(&sync_dir) {
            for f in rd.flatten() {
                let path = f.path();
                let Some(repo) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                let Some(repo) = repo.strip_suffix(".db") else { continue };
                if configured.as_ref().is_some_and(|c| !c.iter().any(|r| r == repo)) {
                    continue;
                }
                let bytes = match fs::read(&path) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        db_errors.push(format!("{repo}: {e}"));
                        continue;
                    }
                };
                match db::parse_sync_db(&bytes, repo) {
                    Ok(pkgs) => {
                        by_repo.insert(repo.to_string(), pkgs);
                    }
                    Err(e) => db_errors.push(format!("{repo}: {e}")),
                }
                if let Ok(meta) = f.metadata()
                    && let Ok(t) = meta.modified()
                {
                    fresh.insert(repo.to_string(), t);
                }
            }
        }

        // Repositories in the order the config declares them, which is the
        // order pacman resolves a name in; without a config to read, by name,
        // so the same system still reads the same way.
        let mut order: Vec<String> = match configured {
            Some(repos) => repos
                .into_iter()
                .filter(|r| by_repo.contains_key(r.as_str()))
                .collect(),
            None => {
                let mut all: Vec<String> = by_repo.keys().cloned().collect();
                all.sort();
                all
            }
        };
        order.dedup();

        let mut index: HashMap<String, usize> = HashMap::new();
        let mut entries: Vec<Entry> = Vec::new();
        for repo in &order {
            for p in by_repo.remove(repo.as_str()).unwrap_or_default() {
                // One row per name: the first repository that carries it owns
                // the row, because that copy is what an install or an update
                // would bring. A second row for the same name read as a
                // package that is both installed and available to install.
                if index.contains_key(&p.name) {
                    continue;
                }
                index.insert(p.name.clone(), entries.len());
                entries.push(Entry {
                    description: p.desc.clone(),
                    repository: Some(p.repository.clone()),
                    installed: None,
                    name: p.name.clone(),
                    sync: Some(p),
                });
            }
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

        let db_fresh = order.iter().filter_map(|r| fresh.get(r.as_str()).copied()).max();
        let dependents = dependents_index(&entries);
        let (provided_installed, provided_repo) = provides_index(&entries);

        Ok(Model {
            entries,
            index,
            dependents,
            provided_installed,
            provided_repo,
            db_fresh,
            db_errors,
        })
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
            .filter(|e| e.status() == Status::UpdateAvailable)
            .collect()
    }

    /// The installed names no configured repository carries — `pacman -Qqm`'s
    /// set, with the same rule the model uses everywhere else: a database the
    /// config does not name makes a package foreign. These are the only names
    /// an AUR check can mean anything for: a name a repository carries is
    /// checked by Refresh, which is already automatic.
    pub fn foreign_names(&self) -> Vec<String> {
        let mut hits: Vec<String> = self
            .entries
            .iter()
            .filter(|e| e.installed.is_some() && e.repository.is_none())
            .map(|e| e.name.clone())
            .collect();
        hits.sort();
        hits
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
            .filter(|e| e.installed.is_none() && e.repository.as_deref() == Some(DISTRO_REPO))
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

    /// Which installed packages need `name` — by that name or by one of its
    /// provisions. This is the rule `-Rns` itself respects: a removal that
    /// breaks a dependency, spelled either way, fails in pacman, so anything
    /// that plans a removal asks this first. The names are reported, not a
    /// yes/no, because a *set* of removals resolves together: a name needed
    /// only by another member of the set is still removable.
    pub fn needed_by_names(&self, name: &str) -> Vec<String> {
        let mut out = self.reverse_deps(name);
        if let Some(l) = self.installed(name) {
            for p in &l.provides {
                out.extend(self.reverse_deps(db::dep_name(p)));
            }
        }
        out.sort();
        out.dedup();
        out
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

    /// Whether a dependency string is already answered on this system — by an
    /// installed package of that name or by one that provides it. The name is
    /// matched without its version comparator: the cost of ignoring the
    /// version is asking for something extra, never a build that then fails
    /// (makepkg asks alpm, which does check it).
    pub fn dep_satisfied(&self, dep: &str) -> bool {
        let dep = db::dep_name(dep);
        self.installed(dep).is_some() || self.provided_installed.contains(dep)
    }

    /// Whether one of the configured repositories carries this name, by name
    /// or by provision. This is the question `Model::load` already answered
    /// when it decided which rows exist, so an AUR dependency that is really
    /// a repository package is installed with `pacman -S` rather than built
    /// from source.
    pub fn in_repo(&self, name: &str) -> bool {
        let name = db::dep_name(name);
        self.get(name).is_some_and(|e| e.sync.is_some()) || self.provided_repo.contains(name)
    }

    /// The packages a repository entry would bring that this system does not
    /// have — the entry's own `%DEPENDS%`, minus what is installed.
    ///
    /// This is how `base` and `base-devel` work on Arch: they are real
    /// metapackages in the repositories, not pacman groups (measured:
    /// `pacman -Qg base-devel` finds nothing, while core.db's
    /// `base-devel-1-2/desc` carries the tool list in `%DEPENDS%`).
    ///
    /// `None` means there is nothing to report: the entry is satisfied, or no
    /// loaded repository carries it. A caller must not read that as "it is
    /// missing".
    pub fn missing_dependencies(&self, entry: &str) -> Option<Vec<String>> {
        let members = self.get(entry)?.sync.as_ref()?.depends.clone();
        if members.is_empty() {
            return None;
        }
        let missing: Vec<String> = members
            .iter()
            .filter(|m| !self.dep_satisfied(m))
            .map(|m| db::dep_name(m).to_string())
            .collect();
        (!missing.is_empty()).then_some(missing)
    }

    /// For a set of names about to be installed, the dependencies that are
    /// virtuals with several possible providers — the questions pacman would
    /// otherwise answer by silently taking provider #1.
    ///
    /// Each requested sync entry's `%DEPENDS%` is read; a dependency already
    /// answered on the system (installed by name or provision) is skipped,
    /// because pacman would not ask about it either. A name no repository
    /// package *is* but some packages *provide* is a virtual; its candidates
    /// come back in pacman's own order — the `entries` order is repository
    /// configuration order, then database order within a repository, the same
    /// rule `Model::load` took from `-Sp` — minus candidates already installed.
    /// One candidate is not a question: pacman takes it without asking.
    pub fn unanswered_providers(&self, names: &[String]) -> Vec<ProviderQuestion> {
        let mut questions: Vec<ProviderQuestion> = Vec::new();
        let mut asked: HashSet<&str> = HashSet::new();
        for name in names {
            let Some(requested) = self.get(name) else { continue };
            // A local-only package has no %DEPENDS% to read; its requirements
            // are whatever pacman resolves at install time, same as ever.
            let Some(sync) = requested.sync.as_ref() else { continue };
            for dep in &sync.depends {
                let dep = db::dep_name(dep);
                if self.dep_satisfied(dep) || !asked.insert(dep) {
                    continue;
                }
                // A concrete name resolves directly; only a virtual asks.
                if self.get(dep).is_some_and(|e| e.sync.is_some()) {
                    continue;
                }
                let candidates: Vec<ProviderChoice> = self
                    .entries
                    .iter()
                    .filter(|e| {
                        e.installed.is_none()
                            && e.sync.as_ref().is_some_and(|s| {
                                s.provides.iter().any(|p| db::dep_name(p) == dep)
                            })
                    })
                    .filter_map(|e| {
                        let s = e.sync.as_ref()?;
                        Some(ProviderChoice {
                            name: s.name.clone(),
                            repository: s.repository.clone(),
                            csize: s.csize,
                        })
                    })
                    .collect();
                if candidates.len() > 1 {
                    questions.push(ProviderQuestion {
                        virtual_name: dep.to_string(),
                        candidates,
                    });
                }
            }
        }
        questions
    }

    /// The AUR answers that are updates for this system: installed, and
    /// older than what the AUR would build. A row the AUR flags out of date
    /// *without* a newer version is not here — rebuilding it would produce
    /// the version already on the system — though its row still carries that
    /// tag. Sorted by name: this list is read as a whole, and the AUR's
    /// ranking means nothing for names this system already has.
    pub fn aur_updates(&self, hits: &[aur::Hit]) -> Vec<AurRow> {
        let mut rows: Vec<AurRow> = self
            .aur_rows(hits)
            .into_iter()
            .filter(|row| row.state == AurState::Update)
            .collect();
        rows.sort_by(|a, b| a.hit.name.cmp(&b.hit.name));
        rows
    }

    /// What an AUR answer means for this system: the same installed / not
    /// installed / up-to-date question the sync rows answer, against the
    /// version the AUR says it would build. The order of `hits` is the AUR's
    /// own ranking and is kept.
    pub fn aur_rows(&self, hits: &[aur::Hit]) -> Vec<AurRow> {
        hits.iter()
            .map(|hit| {
                let installed = self
                    .get(&hit.name)
                    .and_then(|e| e.installed.as_ref())
                    .map(|l| l.version.clone());
                let state = match &installed {
                    None => AurState::NotInstalled,
                    Some(version) => {
                        if vercmp(&hit.version, version) == std::cmp::Ordering::Greater {
                            AurState::Update
                        } else {
                            AurState::Installed
                        }
                    }
                };
                AurRow {
                    hit: hit.clone(),
                    installed_version: installed,
                    state,
                }
            })
            .collect()
    }
}

/// One virtual a request needs answered, and the packages that could answer
/// it — in pacman's own order, the first being what pacman would pick
/// silently. The candidate carries its display facts so the dialog never
/// reaches back into the model.
#[derive(Clone, Debug)]
pub struct ProviderQuestion {
    pub virtual_name: String,
    pub candidates: Vec<ProviderChoice>,
}

#[derive(Clone, Debug)]
pub struct ProviderChoice {
    pub name: String,
    pub repository: String,
    pub csize: u64,
}

/// What an AUR package with this name is on this system.
#[derive(Clone, Debug)]
pub struct AurRow {
    pub hit: aur::Hit,
    pub installed_version: Option<String>,
    pub state: AurState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AurState {
    /// Not on the system: installing it means building it.
    NotInstalled,
    /// Installed at the same or a newer version. Nothing is offered: there is
    /// no version here to move to.
    Installed,
    /// Installed, and the AUR carries a newer version. Updating it is a
    /// rebuild — the AUR publishes no binaries.
    Update,
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

/// provided-by-an-installed-package × provided-by-a-configured-repository. A
/// provision is matched by its leading token for the same reason a dependency
/// is: `libp7zip.so=1-64` is a soname with a version, not a name.
fn provides_index(entries: &[Entry]) -> (HashSet<String>, HashSet<String>) {
    let mut installed = HashSet::new();
    let mut repo = HashSet::new();
    for e in entries {
        if let Some(l) = e.installed.as_ref() {
            for p in &l.provides {
                installed.insert(db::dep_name(p).to_string());
            }
        }
        if let Some(s) = e.sync.as_ref() {
            for p in &s.provides {
                repo.insert(db::dep_name(p).to_string());
            }
        }
    }
    (installed, repo)
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
            provides: vec![],
        }
    }

    fn local(name: &str, version: &str, reason: Reason, depends: &[&str]) -> LocalPackage {
        LocalPackage {
            name: name.into(),
            version: version.into(),
            desc: String::new(),
            reason,
            depends: depends.iter().map(|s| s.to_string()).collect(),
            provides: vec![],
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
        let (provided_installed, provided_repo) = provides_index(&entries);
        Model {
            entries,
            index,
            dependents,
            provided_installed,
            provided_repo,
            db_fresh: None,
            db_errors: Vec::new(),
        }
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
    fn the_apps_own_package_is_listed_but_never_removed() {
        // It is a package like any other and needs its updates, so it is in
        // every list; only its removal is refused. It also stays a real
        // dependent, so removing one of its dependencies still says so.
        let m = model(vec![
            entry(
                SELF_PACKAGE,
                Some(local(SELF_PACKAGE, "0.1.0-1", Reason::Explicit, &["polkit"])),
                Some(sync(SELF_PACKAGE, "0.2.0-1", "the package manager")),
            ),
            entry("htop", Some(local("htop", "3.0-1", Reason::Explicit, &[])), Some(sync("htop", "3.1-1", "monitor"))),
            entry("polkit", Some(local("polkit", "128-1", Reason::Dependency, &[])), None),
        ]);
        // Listed by the Installed page and by Browse, by empty query and by name.
        for scope in [Scope::Installed, Scope::All] {
            assert!(m.search("", scope).iter().any(|e| e.name == SELF_PACKAGE));
            assert!(m.search("aelum", scope).iter().any(|e| e.name == SELF_PACKAGE));
        }
        // 0.1.0-1 < 0.2.0-1, so the app can update itself like any other row.
        assert_eq!(
            m.updatable().iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            [SELF_PACKAGE, "htop"]
        );
        // The one thing the row may not do is remove itself.
        assert!(m.get(SELF_PACKAGE).unwrap().is_self());
        assert!(!m.get(SELF_PACKAGE).unwrap().can_remove());
        assert!(m.get("htop").unwrap().can_remove());
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
            // Not from the distro repo; and the app's own package is only here
            // because this fixture has it uninstalled (run from a source tree).
            entry("firefox", None, Some(sync("firefox", "1.0", "browser"))),
            entry(SELF_PACKAGE, None, Some(aelum(SELF_PACKAGE, "0.2.0-1", "the package manager"))),
        ]);
        let names: Vec<_> = m.distro_apps().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["aelum-newapp", SELF_PACKAGE]);
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
    fn needed_by_names_answers_name_and_provision() {
        // The rule `-Rns` respects: myapp needs java-runtime, which only
        // the installed jdk provides — removing jdk breaks myapp exactly as
        // removing a name-sake dependency would. A build-only name is named
        // by nothing.
        let mut jdk = local("jdk", "17-1", Reason::Dependency, &[]);
        jdk.provides = vec!["java-runtime>=17".into()];
        let m = model(vec![
            entry("myapp", Some(local("myapp", "1.0-1", Reason::Explicit, &["java-runtime>=17", "glibc"])), None),
            entry("jdk", Some(jdk), None),
            entry("glibc", Some(local("glibc", "2.40-1", Reason::Dependency, &[])), None),
            entry("cmake", Some(local("cmake", "4.0-1", Reason::Explicit, &[])), None),
        ]);
        assert_eq!(m.needed_by_names("jdk"), ["myapp"], "needed through its provision");
        assert_eq!(m.needed_by_names("java-runtime"), ["myapp"], "the provision itself");
        assert_eq!(m.needed_by_names("glibc"), ["myapp"], "needed by name");
        assert!(m.needed_by_names("cmake").is_empty(), "nothing names it");
        assert!(m.needed_by_names("absent").is_empty(), "not on the system at all");
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

    /// An AUR answer, as `aur::parse_reply` would hand one over.
    fn hit(name: &str, version: &str) -> aur::Hit {
        aur::Hit {
            name: name.into(),
            version: version.into(),
            description: "built from the AUR".into(),
            url: String::new(),
            url_path: format!("/cgit/aur.git/snapshot/{name}.tar.gz"),
            votes: 12,
            out_of_date: false,
        }
    }

    #[test]
    fn an_aur_update_list_holds_only_rows_with_a_newer_version() {
        let m = model(vec![
            entry("newapp", None, None),
            entry("htop", Some(local("htop", "3.1-1", Reason::Explicit, &[])), None),
            entry("gimp", Some(local("gimp", "2.99-1", Reason::Explicit, &[])), None),
            entry("editor", Some(local("editor", "1.0-2", Reason::Explicit, &[])), None),
        ]);
        let rows = m.aur_updates(&[
            hit("gimp", "3.0-1"),   // installed and older: the one real update
            hit("htop", "3.1-1"),   // the same version: nothing to build
            hit("newapp", "1.0-1"), // not installed: an install, not an update
            hit("editor", "1.0-1"), // older than what is already installed
        ]);
        assert_eq!(
            rows.iter().map(|r| r.hit.name.as_str()).collect::<Vec<_>>(),
            ["gimp"]
        );
    }

    #[test]
    fn foreign_names_are_the_installed_ones_no_repository_carries() {
        // The fixture: firefox is not installed, htop and glibc come from a
        // repository, myapp and its dependency came from nowhere else.
        let m = fixture();
        assert_eq!(m.foreign_names(), ["myapp", "orphan-dep"]);
    }

    #[test]
    fn an_aur_row_answers_the_same_three_questions_as_any_other_row() {
        let m = model(vec![
            // Not on the system at all.
            entry("newapp", None, None),
            // Installed and current: the AUR has nothing to offer.
            entry("htop", Some(local("htop", "3.1-1", Reason::Explicit, &[])), None),
            // Installed, and the AUR would build something newer.
            entry("gimp", Some(local("gimp", "2.99-1", Reason::Explicit, &[])), None),
        ]);
        let rows = m.aur_rows(&[hit("newapp", "1.0-1"), hit("htop", "3.1-1"), hit("gimp", "3.0-1")]);
        assert_eq!(rows[0].state, AurState::NotInstalled);
        assert!(rows[0].installed_version.is_none());
        assert_eq!(rows[1].state, AurState::Installed);
        assert_eq!(rows[1].installed_version.as_deref(), Some("3.1-1"));
        assert_eq!(rows[2].state, AurState::Update);
        // The AUR's own ranking is kept, and the answer is compared with
        // vercmp, not with a string compare.
        assert_eq!(
            rows.iter().map(|r| r.hit.name.as_str()).collect::<Vec<_>>(),
            ["newapp", "htop", "gimp"]
        );
    }

    #[test]
    fn an_installed_provision_satisfies_a_dependency() {
        // Measured in the local DB and in extra.db: a package answers for
        // names that are not its own (7zip → p7zip). A dependency matched by
        // name alone would ask the user to install a package that does not
        // exist, and makepkg would then build something already provided.
        let mut p = local("7zip", "26.05-1", Reason::Explicit, &[]);
        p.provides = vec!["p7zip".into()];
        let mut s = sync("java", "17-1", "jdk");
        s.provides = vec!["java-runtime=17".into()];
        let m = model(vec![
            entry("7zip", Some(p), None),
            entry("java", None, Some(s)),
        ]);
        assert!(m.dep_satisfied("p7zip"));
        assert!(m.dep_satisfied("7zip"));
        assert!(m.in_repo("java-runtime"));
        // A dependency carrying a comparator is still answered by the name:
        // the version is alpm's business when it installs, not ours here.
        assert!(m.in_repo("java-runtime>=17"));
        // Nothing installed answers for it, and nothing in a repository is
        // called `nonsense` either.
        assert!(!m.dep_satisfied("nonsense"));
        assert!(!m.in_repo("nonsense"));
    }

    #[test]
    fn a_metapackage_is_its_dependency_list() {
        // `pacman -Sg base-devel` reads the `base-devel` entry of the sync
        // DB, whose %DEPENDS% is the member list. Only part of it is here.
        let mut group = sync("base-devel", "1-2", "Basic tools to build packages");
        group.depends = vec!["gcc".into(), "make".into(), "patch".into()];
        let m = model(vec![
            entry("base-devel", None, Some(group)),
            entry("gcc", Some(local("gcc", "15-1", Reason::Dependency, &[])), None),
            entry("make", Some(local("make", "4.4-1", Reason::Dependency, &[])), None),
        ]);
        assert_eq!(m.missing_dependencies("base-devel").unwrap(), ["patch"]);
        // A group no loaded repository describes is not a fact about the
        // system, so it is reported as nothing to say.
        assert_eq!(m.missing_dependencies("optdepends"), None);
    }

    #[test]
    fn a_satisfied_metapackage_has_nothing_to_report() {
        let mut group = sync("base-devel", "1-2", "Basic tools");
        group.depends = vec!["make".into()];
        let m = model(vec![
            entry("base-devel", None, Some(group)),
            entry("make", Some(local("make", "4.4-1", Reason::Dependency, &[])), None),
        ]);
        assert_eq!(m.missing_dependencies("base-devel"), None);
    }

    /// A sync entry providing virtuals, with a download size — what the
    /// provider chooser lists.
    fn sync_provides(name: &str, repo: &str, provides: &[&str], csize: u64) -> SyncPackage {
        SyncPackage {
            depends: vec![],
            provides: provides.iter().map(|s| s.to_string()).collect(),
            repository: repo.into(),
            csize,
            ..sync(name, "1.0-1", "a provider")
        }
    }

    #[test]
    fn a_virtual_with_several_providers_asks_once_in_pacman_s_order() {
        // `steam` depends on the virtuals `vulkan-driver` and
        // `lib32-vulkan-driver`, never on a driver by name — and a system
        // with no driver installed gets provider #1 silently. The candidate
        // order is the entries order, which is the order the config resolves
        // names in, so the first candidate is pacman's default.
        let mut steam = sync("steam", "1.1-1", "the platform");
        steam.depends = vec![
            "glibc".into(),
            "vulkan-driver".into(),
            "lib32-vulkan-driver>=1.3".into(),
        ];
        let m = model(vec![
            entry("steam", None, Some(steam)),
            entry("glibc", Some(local("glibc", "2.40-1", Reason::Dependency, &[])), None),
            entry("vulkan-radeon", None, Some(sync_provides("vulkan-radeon", "extra", &["vulkan-driver"], 100))),
            entry("vulkan-intel", None, Some(sync_provides("vulkan-intel", "extra", &["vulkan-driver"], 90))),
            entry("nvidia-utils", None, Some(sync_provides("nvidia-utils", "extra", &["vulkan-driver"], 400))),
            entry("lib32-vulkan-radeon", None, Some(sync_provides("lib32-vulkan-radeon", "multilib", &["lib32-vulkan-driver"], 80))),
            entry("lib32-nvidia-utils", None, Some(sync_provides("lib32-nvidia-utils", "multilib", &["lib32-nvidia-utils"], 300))),
            entry("lib32-mesa", None, Some(sync_provides("lib32-mesa", "multilib", &["lib32-vulkan-driver", "lib32-vulkan-radeon", "lib32-vulkan-intel"], 500))),
        ]);
        let q = m.unanswered_providers(&["steam".into()]);
        assert_eq!(q.len(), 2, "one question per unanswered virtual");
        assert_eq!(q[0].virtual_name, "vulkan-driver");
        assert_eq!(
            q[0].candidates.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["vulkan-radeon", "vulkan-intel", "nvidia-utils"]
        );
        assert_eq!(q[0].candidates[0].repository, "extra");
        assert_eq!(q[0].candidates[0].csize, 100);
        assert_eq!(
            q[1].candidates.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["lib32-vulkan-radeon", "lib32-mesa"],
            "a provider of the versioned virtual only; a provision of another name does not count"
        );
    }

    #[test]
    fn an_installed_provider_or_a_single_candidate_is_never_a_question() {
        // Measured: a virtual with an already-installed provider pulls no
        // vulkan package at all — pacman does not ask. One candidate is not
        // a decision either. A concrete dependency resolves by name.
        let mut radeon = local("vulkan-radeon", "1:25-1", Reason::Dependency, &[]);
        radeon.provides = vec!["vulkan-driver".into()];
        let mut req = sync("requester", "1-1", "asks for things");
        req.depends = vec!["vulkan-driver".into(), "only-one".into(), "glibc".into()];
        let m = model(vec![
            entry("requester", None, Some(req)),
            entry("vulkan-radeon", Some(radeon), None),
            entry("nvidia-utils", None, Some(sync_provides("nvidia-utils", "extra", &["vulkan-driver"], 400))),
            entry("only-one", None, Some(sync_provides("only-one", "extra", &["single-virtual"], 10))),
            entry("other-thing", None, Some(sync("other-thing", "1-1", "not a provider"))),
            entry("glibc", None, Some(sync("glibc", "2.40-1", "libc"))),
        ]);
        let q = m.unanswered_providers(&["requester".into()]);
        assert!(q.is_empty(), "answered, single-candidate, and concrete deps ask nothing");
    }

    #[test]
    fn a_virtual_nothing_provides_asks_nothing() {
        // An unanswerable virtual is pacman's error, not a chooser with zero
        // candidates.
        let mut req = sync("requester", "1-1", "asks");
        req.depends = vec!["nonexistent-virtual".into()];
        let m = model(vec![entry("requester", None, Some(req))]);
        assert!(m.unanswered_providers(&["requester".into()]).is_empty());
        // A name the model does not have at all (an AUR build, a local file)
        // has no %DEPENDS% to read here.
        assert!(m.unanswered_providers(&["some-aur-package".into()]).is_empty());
    }

    #[test]
    fn provider_candidates_follow_the_repository_order_of_the_config() {
        // The same measured rule as the one-row-per-name tests: pacman
        // resolves through pacman.conf, first section wins — and its provider
        // prompt lists candidates in that order. Swapping the sections must
        // flip our list exactly like it flips `-Sp`.
        for (order, first, second) in [
            ("[core]\n[extra]\n", "core-jdk", "extra-jdk"),
            ("[extra]\n[core]\n", "extra-jdk", "core-jdk"),
        ] {
            let name = format!("provider_order_{first}");
            write_pacman_root(
                &name,
                &format!("[options]\n{order}"),
                &[
                    ("core", &[("core-jdk", "17-1", "%PROVIDES%\njava-runtime>=17\n\n",)]),
                    ("extra", &[
                        ("extra-jdk", "17-1", "%PROVIDES%\njava-runtime>=17\n\n"),
                        ("requester", "1-1", "%DEPENDS%\njava-runtime>=17\n\n"),
                    ]),
                ],
                &[],
            );
            let m = load_fixture(&name);
            let q = m.unanswered_providers(&["requester".into()]);
            assert_eq!(q.len(), 1, "config: {order}");
            assert_eq!(
                q[0].candidates.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
                [first, second],
                "config: {order}"
            );
        }
    }

    /// The provider questions must agree with pacman's own prompt on a real
    /// system. Ignored by default: it needs an Arch host with sync databases.
    /// Run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn the_provider_questions_match_pacman_s_prompt() {
        let m = Model::load(Path::new(PACMAN_ROOT), Path::new(PACMAN_CONFIG))
            .expect("real pacman root");
        let ours = m.unanswered_providers(&["steam".into()]);
        // `pacman -Sp steam` prints "There are N providers available for X:"
        // followed by ":: [repo] name" lines — the exact list and order the
        // chooser must show. With every virtual already answered it prints
        // none, and neither may we.
        let out = std::process::Command::new("pacman")
            .args(["-Sp", "steam"])
            .stdin(std::process::Stdio::null())
            .output()
            .expect("pacman");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let mut asked: Vec<(String, Vec<String>)> = Vec::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix(":: There are ")
                && let Some(v) = rest.split(" providers available for ").nth(1)
            {
                let v = v.trim().trim_start_matches('`').trim_end_matches('`').to_string();
                asked.push((v, Vec::new()));
            } else if let Some((_, list)) = asked.last_mut()
                && let Some(rest) = line.strip_prefix(":: [")
                && let Some(name) = rest.split(' ').nth(1)
            {
                list.push(name.to_string());
            }
        }
        let ours: Vec<(String, Vec<String>)> = ours
            .iter()
            .map(|q| (q.virtual_name.clone(), q.candidates.iter().map(|c| c.name.clone()).collect()))
            .collect();
        assert_eq!(ours, asked, "pacman's prompt is the oracle");
    }

    /// A scratch directory for on-disk fixtures. Named per test and per
    /// process so tests running in parallel cannot collide.
    fn fixture_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("aelum-pkg-installer-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    /// One sync-repository package for the fixture writer: name, version,
    /// and any extra desc sections.
    type SyncSpec<'a> = &'a [(&'a str, &'a str, &'a str)];

    /// A pacman root on disk, the way a real system has one: `sync/<repo>.db`
    /// in the compression a mirror sends (gzip) and `local/<name>-<ver>/desc`.
    /// `Model::load` reads it as written, so these tests hold the whole path
    /// the Browse page's rows come from. The third sync field is extra desc
    /// sections (`"%DEPENDS%\n…\n\n"`) the entry carries beyond the basics.
    fn write_pacman_root(
        name: &str,
        config: &str,
        sync: &[(&str, SyncSpec)],
        local: &[(&str, &str)],
    ) {
        let dir = fixture_dir(name);
        fs::create_dir_all(dir.join("sync")).unwrap();
        fs::create_dir_all(dir.join("local")).unwrap();
        fs::write(dir.join("pacman.conf"), config).unwrap();
        for (repo, pkgs) in sync {
            let mut tar = Vec::new();
            {
                let mut ar = tar::Builder::new(&mut tar);
                for (pkg, version, extra) in *pkgs {
                    let text = format!(
                        "%NAME%\n{pkg}\n\n%VERSION%\n{version}\n\n%DESC%\ntest package\n\n{extra}"
                    );
                    let mut header = tar::Header::new_ustar();
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_size(text.len() as u64);
                    header.set_mode(0o644);
                    header.set_mtime(0);
                    ar.append_data(&mut header, format!("{pkg}/desc"), text.as_bytes())
                        .unwrap();
                }
                ar.finish().unwrap();
            }
            let mut gz = Vec::new();
            {
                let mut enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::default());
                std::io::Write::write_all(&mut enc, &tar).unwrap();
                enc.finish().unwrap();
            }
            fs::write(dir.join("sync").join(format!("{repo}.db")), gz).unwrap();
        }
        for (pkg, version) in local {
            let dir = dir.join("local").join(format!("{pkg}-{version}"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("desc"),
                format!("%NAME%\n{pkg}\n\n%VERSION%\n{version}\n\n%REASON%\n1\n\n"),
            )
            .unwrap();
        }
    }

    fn load_fixture(name: &str) -> Model {
        let dir = std::env::temp_dir().join(format!("aelum-pkg-installer-{name}-{}", std::process::id()));
        let m = Model::load(&dir, &dir.join("pacman.conf")).expect("fixture loads");
        let _ = fs::remove_dir_all(&dir);
        m
    }

    #[test]
    fn a_package_in_two_repositories_is_one_row() {
        // What the user saw on a fresh install: every package appeared twice,
        // once labelled `[aelum-offline]` with an Install button and once
        // `[extra]` with "Installed". The install's `pacstrap` runs through the
        // live session's pacman.conf, which declares `[aelum-offline]`, and
        // pacman writes the database it resolved from into the *target's* sync
        // dir — where the installed system's own config no longer names it.
        // pacman has never seen that database; neither may this app.
        write_pacman_root(
            "two_repos",
            "[options]\n[core]\n[extra]\n[aelum]\n",
            &[
                ("aelum-offline", &[("adwaita-icon-theme", "51.0-1", "")]),
                ("extra", &[("adwaita-icon-theme", "51.0-1", "")]),
            ],
            &[("adwaita-icon-theme", "51.0-1")],
        );
        let m = load_fixture("two_repos");
        assert_eq!(m.search("", Scope::All).len(), 1, "one package, one row");
        let e = m.get("adwaita-icon-theme").unwrap();
        assert_eq!(e.repository.as_deref(), Some("extra"));
        // The row is the installed one, not a second "Install" affordance.
        assert_eq!(e.status(), Status::InstalledDep);
        assert!(e.installed.is_some() && e.sync.is_some());
    }

    #[test]
    fn the_row_is_the_copy_the_config_would_install() {
        // Measured against pacman: with two configured repositories carrying
        // one name, `-Sp` and `-Qu` use the copy in the first section, and
        // swapping the sections flips the answer. The row has to follow, or it
        // shows a version the system cannot install.
        for (order, want_repo, want_version) in [
            ("[extra]\n[aelum]\n", "extra", "1.0-1"),
            ("[aelum]\n[extra]\n", "aelum", "2.0-1"),
        ] {
            let name = format!("order_{want_repo}");
            write_pacman_root(
                &name,
                &format!("[options]\n{order}"),
                &[("extra", &[("hello", "1.0-1", "")]), ("aelum", &[("hello", "2.0-1", "")])],
                &[("hello", "0.9-1")],
            );
            let m = load_fixture(&name);
            let e = m.get("hello").unwrap();
            assert_eq!(e.repository.as_deref(), Some(want_repo), "config: {order}");
            assert_eq!(e.display_version(), want_version, "config: {order}");
            // …and that is the only update the Updates page may claim.
            assert_eq!(
                m.updatable().iter().map(|e| e.display_version()).collect::<Vec<_>>(),
                [want_version],
                "config: {order}"
            );
        }
    }

    #[test]
    fn an_unconfigured_database_offers_no_update() {
        // The same leftover on a package that *is* installed: reading it would
        // put a version on the Updates page that `pacman -Syu` never brings.
        write_pacman_root(
            "stale_update",
            "[options]\n[extra]\n",
            &[
                ("aelum-offline", &[("hello", "9.9-1", "")]),
                ("extra", &[("hello", "1.0-1", "")]),
            ],
            &[("hello", "1.0-1")],
        );
        let m = load_fixture("stale_update");
        assert!(m.updatable().is_empty());
        assert_eq!(m.get("hello").unwrap().status(), Status::InstalledDep);
        // The freshness line is about the databases that are shown, not about
        // whatever is lying in the sync dir.
        assert!(m.db_fresh.is_some());
    }

    #[test]
    fn without_a_config_to_read_every_database_is_still_one_row_each() {
        // A missing pacman.conf is not a reason to show nothing — but the
        // duplicate rows are still wrong, so they are merged by name.
        write_pacman_root(
            "no_config",
            "",
            &[("zz-a", &[("hello", "1.0-1", "")]), ("zz-b", &[("hello", "2.0-1", "")])],
            &[],
        );
        let dir = std::env::temp_dir().join(format!(
            "aelum-pkg-installer-no_config-{}",
            std::process::id()
        ));
        let m = Model::load(&dir, &dir.join("absent.conf")).expect("loads anyway");
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(m.search("", Scope::All).len(), 1);
        // Deterministic: the lower-named repository owns the row.
        assert_eq!(m.get("hello").unwrap().repository.as_deref(), Some("zz-a"));
    }

    #[test]
    fn a_repo_the_config_names_but_never_downloaded_is_not_a_channel() {
        // The Aelum page distinguishes "nothing to add" from "the repository
        // is not here". A leftover database from a repository the config does
        // *not* name must not answer that question either.
        write_pacman_root(
            "no_distro_repo",
            "[options]\n[core]\n[extra]\n",
            &[("aelum", &[("aelum-newapp", "1.0-1", "")])],
            &[],
        );
        let m = load_fixture("no_distro_repo");
        assert!(!m.has_distro_repo());
        assert!(m.distro_apps().is_empty());
    }

    /// The classification must agree with pacman's own `-Qe` on a real
    /// system. Ignored by default: it needs an Arch host and shells out
    /// to pacman. Run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn explicit_matches_pacman_qe() {
        let m = Model::load(Path::new("/var/lib/pacman"), Path::new("/etc/pacman.conf"))
            .expect("real pacman root");
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
    ///
    /// This is also the oracle for the repository rules: pacman resolves a
    /// name through pacman.conf — the first configured repository that carries
    /// it, and no database the config does not name — so a model that reads
    /// the sync directory instead of the config disagrees here.
    #[test]
    #[ignore]
    fn updates_match_pacman_qu() {
        let m = Model::load(Path::new("/var/lib/pacman"), Path::new("/etc/pacman.conf"))
            .expect("real pacman root");
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
            .map(str::to_string)
            .collect();
        theirs.sort();
        let mut ours = ours;
        ours.sort();
        assert_eq!(ours, theirs);
    }

    /// The set an AUR update check asks about is pacman's own `-Qqm`: every
    /// installed name no configured repository carries. Ignored by default:
    /// it needs an Arch host. Run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn foreign_names_match_pacman_qqm() {
        let m = Model::load(Path::new(PACMAN_ROOT), Path::new(PACMAN_CONFIG))
            .expect("real pacman root");
        let mut theirs = pacman(&["-Qqm"]);
        theirs.sort();
        assert_eq!(m.foreign_names(), theirs);
    }

    /// The AUR build's one precondition is read the way pacman reads it.
    /// Ignored by default: it needs a real Arch host with sync databases.
    /// Run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn the_build_tools_are_the_ones_pacman_would_install() {
        let m = Model::load(Path::new(PACMAN_ROOT), Path::new(PACMAN_CONFIG))
            .expect("real pacman root");
        // base-devel is a metapackage, so the oracle is what pacman says it
        // depends on — a group listing finds nothing for it.
        let info = String::from_utf8(
            std::process::Command::new("pacman")
                .args(["-Si", "base-devel"])
                .output()
                .expect("pacman")
                .stdout,
        )
        .unwrap();
        let members: Vec<String> = info
            .lines()
            .find(|l| l.starts_with("Depends On"))
            .expect("pacman -Si base-devel answers with a Depends line")
            .split(':')
            .nth(1)
            .expect("a value")
            .split_whitespace()
            .map(str::to_string)
            .collect();
        assert!(!members.is_empty(), "base-devel brings nothing?");
        let installed: HashSet<String> = pacman(&["-Qq"]).into_iter().collect();
        let theirs: Vec<String> = members
            .iter()
            .filter(|name| !installed.contains(*name))
            .cloned()
            .collect();
        let ours = m.missing_dependencies("base-devel").unwrap_or_default();
        assert_eq!(ours, theirs, "what base-devel needs, minus what -Q has");
    }

    /// pacman's own answers, one per line.
    fn pacman(args: &[&str]) -> Vec<String> {
        let out = std::process::Command::new("pacman")
            .args(args)
            .output()
            .expect("pacman");
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// A press of Remove answers both removal questions on the UI thread
    /// before the confirmation appears. Scanning every installed package per
    /// question took ~2 s each on a 1258-package system, so the budget here is
    /// the difference between a dialog and a freeze. Ignored by default: it
    /// needs a real Arch host.
    #[test]
    #[ignore]
    fn removal_questions_answer_on_a_ui_thread_budget() {
        let m = Model::load(Path::new("/var/lib/pacman"), Path::new("/etc/pacman.conf"))
            .expect("real pacman root");
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
