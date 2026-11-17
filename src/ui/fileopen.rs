//! The file-open flow: double-clicking or dropping a `.pkg.tar.zst` shows
//! what the file is and installs it with `pacman -U`. With the manager
//! closed — Files' double-click — the very same dialog stands on its own:
//! that confirmation is the whole interface, the manager is not built.
//!
//! The dialog's other answer is Extract: unpack the archive into a folder
//! named after the package, beside it. That is not archive-manager
//! duplication — no archive manager *can* open a `.pkg.tar.zst` as the
//! archive it is (the MIME type is unknown to it, and its magic-only
//! fallback sees a bare zstd stream and peels just that layer), so this is
//! the only program on the desktop that takes a package apart in one step.
//!
//! One honest caveat, said out loud: pacman does not signature-check
//! local files, so this dialog never claims the package is verified.

use std::path::PathBuf;

use super::app::Ui;
use super::dialog;
use super::human_size;
use crate::db::{self, PkgInfo};
use crate::i18n;
use crate::txn::Txn;

pub fn show(ui: &Ui, info: PkgInfo, path: PathBuf) -> libadwaita::Dialog {
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
    let dest = db::extract_destination(&path);
    details.push(i18n::t_fmt(
        "fileopen.extractTo",
        &[("folder", &dest.display().to_string())],
    ));
    details.push(i18n::t("fileopen.unsigned"));

    let ui_txn = ui.clone();
    let ui_extract = ui.clone();
    let extract_path = path.clone();
    dialog::confirm_with_secondary(
        ui,
        &i18n::t("fileopen.title"),
        &body,
        &details,
        dialog::Actions {
            primary: &i18n::t("action.install"),
            secondary: Some(&i18n::t("action.extract")),
            destructive: false,
        },
        move || ui_extract.extract_package_file(extract_path, dest),
        move || ui_txn.start_txn(Txn::InstallFiles(vec![path])),
    )
}
