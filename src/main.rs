//! Packages — a package manager for the Aelum desktop.
//!
//! Three pages over one in-memory model: the explicitly-installed packages
//! (with a real uninstall), the pending updates (with a per-package Update
//! and Update All), and everything the configured sync repositories carry
//! (with install). At startup the model is read and a database refresh
//! checks for updates — quiet on success, a log dialog on failure. A
//! `.pkg.tar.zst` opened from a file manager lands in the same transaction
//! flow. Privileged operations run through the package's own helper under
//! pkexec — see [`txn`].
//!
//! The model is read from pacman's on-disk state on a worker thread
//! ([`db`], [`model`]); the only subprocesses are the transactions.

mod db;
mod model;
mod txn;
mod ui;
mod vercmp;

use std::rc::Rc;

use gtk::prelude::*;
use libadwaita as adw;

const APP_ID: &str = "aelum.packageinstaller.Packages";

fn main() -> gtk::glib::ExitCode {
    let app = adw::Application::new(
        Some(APP_ID),
        gtk::gio::ApplicationFlags::HANDLES_OPEN,
    );
    let ui = Rc::new(ui::Ui::new());

    app.connect_activate({
        let ui = Rc::clone(&ui);
        move |app| {
            if let Some(window) = ui.window.borrow().as_ref() {
                window.present();
                return;
            }
            ui::window::build(app, &ui);
            ui.load_model();
            ui.auto_check();
        }
    });

    // A file manager hands us the opened files here; the first one gets
    // the dialog, the rest are rare enough to mention rather than stack.
    app.connect_open({
        let ui = Rc::clone(&ui);
        move |app, files, _hint| {
            if let Some(window) = ui.window.borrow().as_ref() {
                window.present();
            } else {
                ui::window::build(app, &ui);
                ui.load_model();
                ui.auto_check();
            }
            for file in files.iter().skip(1) {
                let name = file
                    .basename()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "a package".into());
                ui.toast(&format!("Only one package is opened at a time; ignoring {name}."));
            }
            if let Some(file) = files.first() {
                match file.path() {
                    Some(path) => ui.open_file(path),
                    None => ui.toast("The opened file is not a local file."),
                }
            }
        }
    });

    app.run()
}
