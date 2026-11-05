//! Shared UI state: the model handle, the transaction gate, and the actions
//! every page reaches for (confirm, run, toast, reload).

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

use futures::StreamExt;
use gtk::glib;
use gtk::prelude::*;
use libadwaita as adw;

use crate::db;
use crate::model::Model;
use crate::txn::{self, Txn, TxnEvent};

/// Where the model lives on every Aelum system.
const PACMAN_ROOT: &str = "/var/lib/pacman";
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
    /// The Updates tab, for the needs-attention badge, and whether the
    /// user is looking at it right now (no badge for what is on screen).
    pub updates_tab: Rc<RefCell<Option<adw::ViewStackPage>>>,
    pub on_updates_page: Rc<Cell<bool>>,
    /// The header's Refresh button; it doubles as the update-check
    /// indicator (a spinner while any refresh runs).
    pub refresh: Rc<RefCell<Option<gtk::Button>>>,
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
            updates_tab: Rc::new(RefCell::new(None)),
            on_updates_page: Rc::new(Cell::new(false)),
            refresh: Rc::new(RefCell::new(None)),
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
            let _ = tx.unbounded_send((seq, Model::load(Path::new(PACMAN_ROOT))));
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
                            ui.toast(&format!(
                                "Could not read {} package database(s): {}",
                                model.db_errors.len(),
                                model.db_errors.join("; ")
                            ));
                        }
                        *ui.model.borrow_mut() = Some(Rc::new(model));
                    }
                    Err(e) => ui.toast(&format!("Could not read the package databases: {e}")),
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
        // The tab badge marks pending updates the user is not already
        // looking at; visiting the page clears it (window.rs).
        let want = !model.updatable().is_empty() && !self.on_updates_page.get();
        if let Some(tab) = self.updates_tab.borrow().as_ref() {
            tab.set_needs_attention(want);
        }
    }

    /// The Refresh button shows a spinner while a refresh — its own press
    /// or the startup check — is running.
    pub fn set_refresh_spinning(&self, on: bool) {
        let Some(button) = self.refresh.borrow().clone() else {
            return;
        };
        if on {
            let spinner = gtk::Spinner::new();
            spinner.set_spinning(true);
            button.set_child(Some(&spinner));
        } else {
            button.set_child(Some(&gtk::Image::from_icon_name("view-refresh-symbolic")));
        }
    }

    /// Run a transaction through the polkit-gated helper, streaming its
    /// output into a log dialog. One at a time: pacman's DB lock makes a
    /// second concurrent run a guaranteed failure, and a gate is kinder
    /// than a lock error.
    pub fn start_txn(&self, txn: Txn) {
        // A refresh never opens a log dialog: the dialog would cover the
        // window and hide the one thing that says the check is running —
        // the spinning button. Its result is the updated pages and badge;
        // only a failure opens the log.
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
                self.toast("Another operation is already running.");
            }
            return;
        }
        // Every privileged operation runs through the packaged helper —
        // the single polkit action, the aelum-snapshot-restore pattern.
        // Without it there is nothing to authorize, and saying so beats
        // a pkexec error in a log dialog.
        if !std::path::Path::new(txn::HELPER).exists() {
            if !quiet {
                self.toast(&format!(
                    "Package operations need the installed aelum-package-installer ({} is missing).",
                    txn::HELPER
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
            self.set_refresh_spinning(true);
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
                            ui.set_refresh_spinning(false);
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
                                ui.toast("Done.");
                            } else if !auto {
                                // The manual check announces itself; the
                                // startup one lets the pages speak.
                                ui.toast("Package databases refreshed.");
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
                self.set_refresh_spinning(false);
            }
            match log {
                Some(log) => log.finish(Err(-1)),
                None => self.toast(&format!("Could not start the operation: {e}")),
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
}
