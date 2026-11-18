//! Packages — a package manager for the Aelum desktop.
//!
//! Five pages over one in-memory model: the explicitly-installed packages
//! (with a real uninstall), everything the configured sync repositories and
//! the distro's own `[aelum]` repository carry (searched, installed), the AUR
//! (searched when asked, built on this machine), and the pending updates. At
//! startup the model is read and, unless the databases were last checked
//! minutes ago ([`cache`]), a database refresh checks for updates — quiet on
//! success, a log dialog on failure. A `.pkg.tar.zst` double-clicked in a
//! file manager arrives as the install confirmation popup alone, no manager;
//! dropped on the manager's window it lands over it — both end in the same
//! transaction flow. Privileged operations run through the package's own
//! helper under pkexec — see [`txn`].
//!
//! The model is read from pacman's on-disk state on a worker thread
//! ([`db`], [`model`]). The only subprocesses are the transactions, which
//! need root ([`txn`]), and the AUR's half of an install — the RPC, the
//! PKGBUILD download, and makepkg — which do not ([`remote`], [`aur`]).

mod aur;
mod cache;
mod countries;
mod db;
mod i18n;
mod model;
mod progress;
mod remote;
mod txn;
mod ui;
mod vercmp;

use std::rc::Rc;

use gtk::gio::prelude::ActionMapExtManual as _;
use gtk::prelude::*;
use libadwaita as adw;

pub(crate) const APP_ID: &str = "aelum.packageinstaller.Packages";

fn main() -> gtk::glib::ExitCode {
    // Locale + catalogs, before any string is built.
    i18n::init();

    let app = adw::Application::new(Some(APP_ID), gtk::gio::ApplicationFlags::HANDLES_OPEN);
    let ui = Rc::new(ui::Ui::new());
    // The popup flow keeps the application handle: its dialogs are windows
    // the application does not know about, and it holds the process up
    // while they are shown (`Ui::present_dialog`).
    *ui.app.borrow_mut() = Some(app.clone());

    // The "⋯" menu's owners (window.rs). Application actions, not window
    // actions: the manager's window is one-shot, and the acts — the mirror
    // update, About, and leaving — outlive any single window. The
    // application handle arrives as the callback's own argument, so no
    // closure has to keep one.
    app.add_action_entries([
        {
            let ui = Rc::clone(&ui);
            gtk::gio::ActionEntry::builder("update-mirrors")
                .activate(move |_app: &adw::Application, _action, _parameter| {
                    ui.update_mirrors();
                })
                .build()
        },
        {
            let ui = Rc::clone(&ui);
            gtk::gio::ActionEntry::builder("about")
                .activate(move |_app: &adw::Application, _action, _parameter| {
                    ui.show_about();
                })
                .build()
        },
        gtk::gio::ActionEntry::builder("quit")
            .activate(move |app: &adw::Application, _action, _parameter| app.quit())
            .build(),
    ]);

    app.connect_activate({
        let ui = Rc::clone(&ui);
        move |app| {
            if let Some(window) = ui.window.borrow().as_ref() {
                window.present();
                return;
            }
            ui::window::build(app, &ui);
            // Arming the startup check while the first model load is on its way
            // is what lets that read decide whether a check is owed at all.
            ui.load_model();
            ui.auto_check();
        }
    });

    // A file manager hands us the opened files here. With the manager
    // open, its window comes forward and the first file gets the dialog
    // over it. With none — the double-click the custom MIME routes here —
    // the confirmation runs as a popup of its own and the manager is
    // never built: no pages to fill, no databases to read, no check to
    // ask authorization for.
    app.connect_open({
        let ui = Rc::clone(&ui);
        move |_app, files, _hint| match ui.window.borrow().clone() {
            Some(window) => {
                window.present();
                ui.open_files(files);
            }
            None => ui.open_files_popup(files.to_vec()),
        }
    });

    app.run()
}
