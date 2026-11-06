//! Shared UI state: the model handle, the transaction gate, and the actions
//! every page reaches for (confirm, run, toast, reload).

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

use futures::StreamExt;
use gtk::glib;
use gtk::prelude::*;
use libadwaita as adw;

use crate::aur;
use crate::db;
use crate::i18n;
use crate::model::{Model, PACMAN_CONFIG, PACMAN_ROOT};
use crate::remote;
use crate::txn::{self, Txn, TxnEvent};

/// Rendered rows per page. A full sync DB is ~15k entries; building that
/// many rows per keystroke is not a cost worth paying — the cap plus the
/// "keep typing" row is what `pacman -Ss` itself does.
pub const MAX_ROWS: usize = 300;

/// Everything the pages share. Cheap to clone (all fields are handles);
/// signal closures hold a copy. The toast overlay is set by
/// [`super::window::build`] — libadwaita refuses to build any widget
/// before the application has initialized GTK.
#[derive(Clone)]
pub struct Ui {
    pub overlay: Rc<RefCell<Option<adw::ToastOverlay>>>,
    pub window: Rc<RefCell<Option<adw::ApplicationWindow>>>,
    pub model: Rc<RefCell<Option<Rc<Model>>>>,
    pub running: Rc<Cell<bool>>,
    pub installed: Rc<RefCell<Option<super::installed::Page>>>,
    pub updates: Rc<RefCell<Option<super::updates::Page>>>,
    pub browse: Rc<RefCell<Option<super::browse::Page>>>,
    pub distro: Rc<RefCell<Option<super::distro::Page>>>,
    pub aur_page: Rc<RefCell<Option<super::aur::Page>>>,
    /// The Updates tab, for the needs-attention badge, and whether the
    /// user is looking at it right now (no badge for what is on screen).
    pub updates_tab: Rc<RefCell<Option<adw::ViewStackPage>>>,
    pub on_updates_page: Rc<Cell<bool>>,
    /// The Aelum tab and its dot: raised while the channel carries apps
    /// this system does not have, cleared by visiting the page — the same
    /// acknowledgement rule as the Updates badge.
    pub aelum_tab: Rc<RefCell<Option<adw::ViewStackPage>>>,
    pub on_aelum_page: Rc<Cell<bool>>,
    /// The last AUR answer the user asked for. Nothing is fetched from the
    /// AUR unasked: the app's only automatic network call is `pacman -Sy`,
    /// against the user's own mirrors, and an AUR badge would mean asking a
    /// third-party server about something the user never asked about.
    pub aur: Rc<RefCell<Option<AurResults>>>,
    /// Whether an AUR search is in flight, and which one is still current.
    /// Two searches can overlap; the slower one must not answer the newer
    /// question (the same rule as `load_seq`).
    pub aur_searching: Rc<Cell<bool>>,
    pub aur_seq: Rc<Cell<u64>>,
    /// The header's Refresh button; it doubles as the update-check
    /// indicator (a spinner while any refresh runs).
    pub refresh: Rc<RefCell<Option<gtk::Button>>>,
    /// The page area (the ViewStack, and the "refresh in progress" page
    /// that covers it while a refresh runs — see [`Ui::set_refresh_running`]).
    pub content: Rc<RefCell<Option<gtk::Stack>>>,
    /// Generation counter for model loads. The startup load and the
    /// post-refresh load can overlap; only the newest one may set the
    /// model, or a slow older read could resurrect stale databases.
    pub load_seq: Rc<Cell<u64>>,
    /// When a Refresh last *succeeded* this session. pacman -Sy sends a
    /// conditional request and leaves the .db files untouched when the
    /// mirror says "not modified", so their mtimes cannot prove when the
    /// last check happened — this does.
    pub last_check: Rc<std::cell::RefCell<Option<std::time::SystemTime>>>,
}

impl Ui {
    pub fn new() -> Ui {
        Ui {
            overlay: Rc::new(RefCell::new(None)),
            window: Rc::new(RefCell::new(None)),
            model: Rc::new(RefCell::new(None)),
            running: Rc::new(Cell::new(false)),
            installed: Rc::new(RefCell::new(None)),
            updates: Rc::new(RefCell::new(None)),
            browse: Rc::new(RefCell::new(None)),
            distro: Rc::new(RefCell::new(None)),
            aur_page: Rc::new(RefCell::new(None)),
            updates_tab: Rc::new(RefCell::new(None)),
            on_updates_page: Rc::new(Cell::new(false)),
            aelum_tab: Rc::new(RefCell::new(None)),
            on_aelum_page: Rc::new(Cell::new(false)),
            aur: Rc::new(RefCell::new(None)),
            aur_searching: Rc::new(Cell::new(false)),
            aur_seq: Rc::new(Cell::new(0)),
            refresh: Rc::new(RefCell::new(None)),
            content: Rc::new(RefCell::new(None)),
            load_seq: Rc::new(Cell::new(0)),
            last_check: Rc::new(RefCell::new(None)),
        }
    }

    pub fn toast(&self, msg: &str) {
        if let Some(overlay) = self.overlay.borrow().as_ref() {
            overlay.add_toast(adw::Toast::new(msg));
        }
    }

    /// Parse the databases on a worker thread; the UI stays responsive
    /// through a full sync-DB read. Worker → main messaging is a futures
    /// channel drained by a task on the thread-local main context
    /// (glib 0.22 removed the old `MainContext::channel`).
    pub fn load_model(&self) {
        let seq = self.load_seq.get() + 1;
        self.load_seq.set(seq);
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            let _ = tx.unbounded_send((
                seq,
                Model::load(Path::new(PACMAN_ROOT), Path::new(PACMAN_CONFIG)),
            ));
        });
        let ui = self.clone();
        glib::MainContext::default().spawn_local(async move {
            if let Some((seq, result)) = rx.next().await {
                if seq != ui.load_seq.get() {
                    return; // a newer load is already on its way
                }
                match result {
                    Ok(model) => {
                        // A repository whose database could not be read is
                        // missing from every page, and an empty Updates page
                        // is indistinguishable from an up-to-date system.
                        // Nothing in this app fails silently.
                        if !model.db_errors.is_empty() {
                            ui.toast(&i18n::t_fmt(
                                "toast.dbErrors",
                                &[
                                    ("count", &model.db_errors.len().to_string()),
                                    ("errors", &model.db_errors.join("; ")),
                                ],
                            ));
                        }
                        *ui.model.borrow_mut() = Some(Rc::new(model));
                    }
                    Err(e) => {
                        ui.toast(&i18n::t_fmt("toast.modelLoad", &[("error", &e.to_string())]));
                    }
                }
                ui.refresh_pages();
            }
        });
    }

    pub fn refresh_pages(&self) {
        let model = self.model.borrow().clone();
        let Some(model) = model else { return };
        if let Some(page) = self.installed.borrow().as_ref() {
            page.refresh(self, &model);
        }
        if let Some(page) = self.updates.borrow().as_ref() {
            page.refresh(self, &model);
        }
        if let Some(page) = self.browse.borrow().as_ref() {
            page.refresh(self, &model);
        }
        if let Some(page) = self.distro.borrow().as_ref() {
            page.refresh(self, &model);
        }
        // The AUR page reads its own state, not the model: its rows were
        // classified against the model when the answer arrived.
        if let Some(page) = self.aur_page.borrow().as_ref() {
            page.refresh(self);
        }
        // The tab badge marks pending updates the user is not already
        // looking at; visiting the page clears it (window.rs).
        let want = !model.updatable().is_empty() && !self.on_updates_page.get();
        if let Some(tab) = self.updates_tab.borrow().as_ref() {
            tab.set_needs_attention(want);
        }
        // The Aelum dot follows the same rule: raised while the channel
        // carries apps this system does not have, silent while the user
        // is reading that very page.
        let want = !model.distro_apps().is_empty() && !self.on_aelum_page.get();
        if let Some(tab) = self.aelum_tab.borrow().as_ref() {
            tab.set_needs_attention(want);
        }
    }

    /// A refresh is running — its own press or the startup check. Two things
    /// follow: the Refresh button shows a spinner, and the page area is
    /// replaced by the "refresh in progress" page. Every row on screen is the
    /// state of the databases *before* the check, so while the check runs the
    /// honest content is that a check is running.
    pub fn set_refresh_running(&self, on: bool) {
        if let Some(button) = self.refresh.borrow().clone() {
            if on {
                let spinner = gtk::Spinner::new();
                spinner.set_spinning(true);
                button.set_child(Some(&spinner));
            } else {
                button.set_child(Some(&gtk::Image::from_icon_name("view-refresh-symbolic")));
            }
        }
        if let Some(content) = self.content.borrow().clone() {
            content.set_visible_child_name(if on { "refreshing" } else { "pages" });
        }
    }

    /// Run a transaction through the polkit-gated helper, streaming its
    /// output into a log dialog. One at a time: pacman's DB lock makes a
    /// second concurrent run a guaranteed failure, and a gate is kinder
    /// than a lock error.
    pub fn start_txn(&self, txn: Txn) {
        // A refresh never opens a log dialog: the dialog would cover the
        // window and hide the two things that say the check is running —
        // the spinning button and the progress page over the tabs. Its
        // result is the updated pages and badge; only a failure opens the log.
        let quiet = matches!(txn, Txn::Refresh);
        self.run_txn(txn, quiet, false);
    }

    /// The startup update check: the same Refresh transaction, quiet like
    /// the manual one, and without even the completion toast — a routine
    /// check that succeeds has no business announcing itself. A failure
    /// still shows the log: nothing in this app fails silently.
    pub fn auto_check(&self) {
        // run_txn refuses quietly too; this guard just keeps the check from
        // being attempted at all on a host without the packaged helper.
        if !std::path::Path::new(txn::HELPER).exists() {
            return;
        }
        self.run_txn(Txn::Refresh, true, true);
    }

    fn run_txn(&self, txn: Txn, quiet: bool, auto: bool) {
        if self.running.get() {
            if !quiet {
                self.toast(&i18n::t("toast.busy"));
            }
            return;
        }
        // Every privileged operation runs through the packaged helper —
        // the single polkit action, the aelum-snapshot-restore pattern.
        // Without it there is nothing to authorize, and saying so beats
        // a pkexec error in a log dialog.
        if !std::path::Path::new(txn::HELPER).exists() {
            if !quiet {
                self.toast(&i18n::t_fmt(
                    "toast.noHelper",
                    &[("helper", txn::HELPER)],
                ));
            }
            return;
        }
        let Some(window) = self.window.borrow().clone() else {
            return;
        };
        self.running.set(true);
        let spinning = matches!(txn, Txn::Refresh);
        if spinning {
            self.set_refresh_running(true);
        }
        let summary = txn.summary();
        let log = if quiet {
            None
        } else {
            Some(super::txnlog::show(&window, &summary))
        };

        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let run = txn::run(&txn, tx);
        let ui = self.clone();
        // The task streams into its own handle; `log` stays here for the
        // spawn-failure path.
        let stream_log = log.clone();
        glib::MainContext::default().spawn_local(async move {
            let mut log = stream_log;
            let mut buffered: Vec<String> = Vec::new();
            while let Some(event) = rx.next().await {
                match event {
                    TxnEvent::Line(line) => match log.as_mut() {
                        Some(log) => log.append(&line),
                        None => buffered.push(line),
                    },
                    TxnEvent::Finished(result) => {
                        ui.running.set(false);
                        if spinning {
                            ui.set_refresh_running(false);
                        }
                        // A quiet failure still gets its log: the buffered
                        // output is the explanation.
                        if log.is_none() && result.is_err() {
                            let shown = super::txnlog::show(&window, &summary);
                            for line in buffered.drain(..) {
                                shown.append(&line);
                            }
                            log = Some(shown);
                        }
                        if let Some(log) = &log {
                            log.finish(result);
                        }
                        if result.is_ok() {
                            if spinning {
                                *ui.last_check.borrow_mut() =
                                    Some(std::time::SystemTime::now());
                            }
                            if log.is_some() {
                                ui.toast(&i18n::t("txnlog.done"));
                            } else if !auto {
                                // The manual check announces itself; the
                                // startup one lets the pages speak.
                                ui.toast(&i18n::t("toast.refreshed"));
                            }
                            ui.load_model();
                        }
                        // A failure's text is the log itself; closing the
                        // dialog is the acknowledgement.
                        break;
                    }
                }
            }
        });
        if let Err(e) = run {
            self.running.set(false);
            if spinning {
                self.set_refresh_running(false);
            }
            match log {
                Some(log) => log.finish(Err(-1)),
                None => {
                    self.toast(&i18n::t_fmt("toast.startFailed", &[("error", &e.to_string())]));
                }
            }
        }
    }

    /// The file-open flow: read `.PKGINFO`, confirm, install.
    pub fn open_file(&self, path: std::path::PathBuf) {
        match db::read_pkginfo(&path) {
            Ok(info) => super::fileopen::show(self, info, path),
            Err(e) => self.toast(&format!("Not a readable Arch package: {e}")),
        }
    }

    /// Ask the AUR. The search is the user's action, never the app's: the
    /// AUR is a third-party server, and this desktop is offline-first — an
    /// AUR page that filled itself at startup would make the app need the
    /// internet to say anything true about the system.
    pub fn search_aur(&self, query: &str) {
        let query = query.trim().to_string();
        let seq = self.aur_seq.get() + 1;
        self.aur_seq.set(seq);
        if query.is_empty() {
            self.aur_searching.set(false);
            *self.aur.borrow_mut() = None;
            self.refresh_pages();
            return;
        }
        self.aur_searching.set(true);
        self.refresh_pages();

        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            let _ = tx.unbounded_send((seq, aur_hits(&query)));
        });
        let ui = self.clone();
        glib::MainContext::default().spawn_local(async move {
            if let Some((seq, result)) = rx.next().await {
                // A newer search is already on screen; this answer is stale.
                if seq != ui.aur_seq.get() {
                    return;
                }
                ui.aur_searching.set(false);
                // The answer is kept as the AUR gave it. Rows are classified
                // against the model when the page is drawn, so they say what
                // these names are on this system *now* — and a search that
                // arrives before the databases have been read is not
                // misclassified as "nothing matches".
                *ui.aur.borrow_mut() = match result {
                    Ok(hits) => Some(AurResults { hits, error: None }),
                    Err(e) => Some(AurResults {
                        hits: Vec::new(),
                        error: Some(e),
                    }),
                };
                ui.refresh_pages();
            }
        });
    }

    /// Build one AUR package and install what the build produces. The whole
    /// request is one job in one log dialog: repository dependencies first
    /// through the same polkit action as everything else, then makepkg as
    /// this user, then `pacman -U` for the files it made.
    pub fn install_aur(&self, hit: aur::Hit) {
        if self.running.get() {
            self.toast(&i18n::t("toast.busy"));
            return;
        }
        if !Path::new(txn::HELPER).exists() {
            self.toast(&i18n::t_fmt("toast.noHelper", &[("helper", txn::HELPER)]));
            return;
        }
        // makepkg needs base-devel, which `base` does not include.
        // Refusing before anything runs is what the AUR helpers do, and it is
        // a sentence the user can act on rather than a makepkg error two
        // minutes into a build.
        let missing = self
            .model
            .borrow()
            .as_ref()
            .and_then(|m| m.missing_dependencies(aur::BUILD_TOOLS));
        if missing.is_some() {
            self.toast(&i18n::t("aur.noBuildTools"));
            return;
        }
        let Some(window) = self.window.borrow().clone() else {
            return;
        };
        self.running.set(true);
        let log = super::txnlog::show(&window, &i18n::t_fmt("txn.summaryBuild", &[("name", &hit.name)]));

        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let spawned = std::thread::Builder::new().spawn(move || remote::aur_install(&hit, &tx));
        if let Err(e) = spawned {
            self.running.set(false);
            log.finish(Err(-1));
            self.toast(&i18n::t_fmt("toast.startFailed", &[("error", &e.to_string())]));
            return;
        }
        let ui = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let mut answered = false;
            while let Some(event) = rx.next().await {
                match event {
                    TxnEvent::Line(line) => log.append(&line),
                    TxnEvent::Finished(result) => {
                        answered = true;
                        ui.running.set(false);
                        log.finish(result);
                        // The new packages are only real once the model has
                        // read them: the pages move because the databases moved.
                        if result.is_ok() {
                            ui.load_model();
                        }
                        break;
                    }
                }
            }
            if !answered {
                // The worker went away without a status — it panicked. A
                // dialog that spins forever and a gate that stays closed are
                // both worse than a failure.
                ui.running.set(false);
                log.finish(Err(-1));
            }
        });
    }
}

/// One AUR search, on the worker thread: the request, then its answer read
/// into rows. An error here is the server's own sentence (the AUR refuses a
/// query that matches too much) and is shown as it came back.
fn aur_hits(query: &str) -> Result<Vec<aur::Hit>, String> {
    let body = remote::http_get(&aur::search_url(query))?;
    match aur::parse_reply(&body)? {
        aur::Response::Search(hits) => Ok(hits),
        aur::Response::Info(_) => Err("the AUR answered a different question".into()),
    }
}

/// What the AUR page shows: the answer to the last search, or the reason
/// there is none. An empty list and a refused request are different facts —
/// the page says which one it has.
pub struct AurResults {
    pub hits: Vec<aur::Hit>,
    pub error: Option<String>,
}
