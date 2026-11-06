//! Packages — a package manager for the Aelum desktop.
//!
//! Five pages over one in-memory model: the explicitly-installed packages
//! (with a real uninstall), everything the configured sync repositories and
//! the distro's own `[aelum]` repository carry (searched, installed), the AUR
//! (searched when asked, built on this machine), and the pending updates. At
//! startup the model is read and a database refresh checks for updates —
//! quiet on success, a log dialog on failure. A `.pkg.tar.zst` opened from a
//! file manager lands in the same transaction flow. Privileged operations run
//! through the package's own helper under pkexec — see [`txn`].
//!
//! The model is read from pacman's on-disk state on a worker thread
//! ([`db`], [`model`]). The only subprocesses are the transactions, which
//! need root ([`txn`]), and the AUR's half of an install — the RPC, the
//! PKGBUILD download, and makepkg — which do not ([`remote`], [`aur`]).

mod aur;
mod db;
mod i18n;
mod model;
mod progress;
mod remote;
mod txn;
mod ui;
mod vercmp;

use std::rc::Rc;

use gtk::prelude::*;
use libadwaita as adw;

const APP_ID: &str = "aelum.packageinstaller.Packages";

fn main() -> gtk::glib::ExitCode {
    // Locale + catalogs, before any string is built.
    i18n::init();

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
                    .unwrap_or_else(|| i18n::t("toast.openFallbackName"));
                ui.toast(&i18n::t_fmt("toast.openExtra", &[("name", &name)]));
            }
            if let Some(file) = files.first() {
                match file.path() {
                    Some(path) => ui.open_file(path),
                    None => ui.toast(&i18n::t("toast.openNotLocal")),
                }
            }
        }
    });

    app.run()
}
