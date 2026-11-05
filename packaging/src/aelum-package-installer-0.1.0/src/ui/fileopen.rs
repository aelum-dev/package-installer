//! The file-open flow: double-clicking a `.pkg.tar.zst` shows what the
//! file is and installs it with `pacman -U`.
//!
//! One honest caveat, said out loud: pacman does not signature-check
//! local files, so this dialog never claims the package is verified.

use std::path::PathBuf;

use super::app::Ui;
use super::dialog;
use super::human_size;
use crate::db::PkgInfo;
use crate::txn::Txn;

pub fn show(ui: &Ui, info: PkgInfo, path: PathBuf) {
    let ui = ui.clone();
    let Some(window) = ui.window.borrow().clone() else {
        return;
    };

    let body = format!(
        "{} {}\n{}",
        info.name,
        info.version,
        if info.desc.is_empty() { "(no description)" } else { &info.desc }
    );
    let mut details = Vec::new();
    if !info.url.is_empty() {
        details.push(info.url.clone());
    }
    if info.isize > 0 {
        details.push(format!("Installed size: {}.", human_size(info.isize)));
    }
    if !info.depends.is_empty() {
        details.push("Missing dependencies will be installed from the sync repositories.".into());
    }
    details.push("Local package files are not signature-checked.".into());

    dialog::confirm(
        &window,
        "Install this package?",
        &body,
        &details,
        "Install",
        false,
        move || ui.start_txn(Txn::InstallFile(path)),
    );
}
