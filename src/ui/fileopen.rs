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
use crate::i18n;
use crate::txn::Txn;

pub fn show(ui: &Ui, info: PkgInfo, path: PathBuf) {
    let ui = ui.clone();
    let Some(window) = ui.window.borrow().clone() else {
        return;
    };

    let desc = if info.desc.is_empty() {
        i18n::t("fileopen.noDesc")
    } else {
        info.desc.clone()
    };
    let body = i18n::t_fmt(
        "fileopen.body",
        &[
            ("name", &info.name),
            ("version", &info.version),
            ("desc", &desc),
        ],
    );
    let mut details = Vec::new();
    if !info.url.is_empty() {
        details.push(info.url.clone());
    }
    if info.isize > 0 {
        details.push(i18n::t_fmt(
            "dialog.installedSize",
            &[("size", &human_size(info.isize))],
        ));
    }
    if !info.depends.is_empty() {
        details.push(i18n::t("fileopen.missingDeps"));
    }
    details.push(i18n::t("fileopen.unsigned"));

    dialog::confirm(
        &window,
        &i18n::t("fileopen.title"),
        &body,
        &details,
        &i18n::t("action.install"),
        false,
        move || ui.start_txn(Txn::InstallFiles(vec![path])),
    );
}
