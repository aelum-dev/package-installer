//! Shared UI state: the model handle, the transaction gate, and the actions
//! every page reaches for (confirm, run, toast, reload).

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

use futures::StreamExt;
use gtk::glib;
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
    pub browse: Rc<RefCell<Option<super::browse::Page>>>,
}

impl Ui {
    pub fn new() -> Ui {
        Ui {
            overlay: Rc::new(RefCell::new(None)),
            window: Rc::new(RefCell::new(None)),
            model: Rc::new(RefCell::new(None)),
            running: Rc::new(Cell::new(false)),
            installed: Rc::new(RefCell::new(None)),
            browse: Rc::new(RefCell::new(None)),
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
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        std::thread::spawn(move || {
            let _ = tx.unbounded_send(Model::load(Path::new(PACMAN_ROOT)));
        });
        let ui = self.clone();
        glib::MainContext::default().spawn_local(async move {
            if let Some(result) = rx.next().await {
                match result {
                    Ok(model) => *ui.model.borrow_mut() = Some(Rc::new(model)),
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
        if let Some(page) = self.browse.borrow().as_ref() {
            page.refresh(self, &model);
        }
    }

    /// Run a transaction through the polkit-gated helper, streaming its
    /// output into a log dialog. One at a time: pacman's DB lock makes a
    /// second concurrent run a guaranteed failure, and a gate is kinder
    /// than a lock error.
    pub fn start_txn(&self, txn: Txn) {
        if self.running.get() {
            self.toast("Another operation is already running.");
            return;
        }
        let Some(window) = self.window.borrow().clone() else {
            return;
        };
        self.running.set(true);
        let log = super::txnlog::show(&window, &txn.summary());

        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let run = txn::run(&txn, tx);
        let ui = self.clone();
        let pump = log.clone();
        glib::MainContext::default().spawn_local(async move {
            while let Some(event) = rx.next().await {
                match event {
                    TxnEvent::Line(line) => pump.append(&line),
                    TxnEvent::Finished(result) => {
                        ui.running.set(false);
                        pump.finish(result);
                        if let Ok(()) = result {
                            ui.toast("Done.");
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
            log.finish(Err(-1));
            self.toast(&format!("Could not start the operation: {e}"));
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
