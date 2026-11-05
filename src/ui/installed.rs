//! The Installed page: explicitly-installed packages with a real uninstall.
//!
//! "Explicitly installed" is pacman's own distinction (`%REASON%`, the `-Qe`
//! view): the packages the user chose, without their dependency trees.

use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use super::app::{MAX_ROWS, Ui};
use super::dialog;
use super::markup_escape;
use crate::model::{Model, Scope, Status};
use crate::txn::Txn;

#[derive(Clone)]
pub struct Page {
    search: gtk::SearchEntry,
    list: gtk::ListBox,
    stack: gtk::Stack,
    banner: adw::Banner,
}

pub fn build(ui: &Ui) -> (Page, gtk::Box) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search installed packages"));
    search.set_margin_start(12);
    search.set_margin_end(12);
    search.set_margin_top(12);
    root.append(&search);

    let banner = adw::Banner::builder().button_label("Update All").build();
    banner.set_margin_top(6);
    {
        let ui = ui.clone();
        banner.connect_button_clicked(move |_| ui.start_txn(Txn::Upgrade));
    }
    root.append(&banner);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_margin_start(12);
    list.set_margin_end(12);
    list.set_margin_bottom(12);
    list.add_css_class("boxed-list");

    let empty = adw::StatusPage::builder()
        .icon_name("package-x-generic-symbolic")
        .title("Reading package databases…")
        .build();

    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    stack.add_named(&list, Some("list"));
    stack.add_named(&empty, Some("empty"));
    stack.set_visible_child_name("empty");
    root.append(&stack);

    let page = Page {
        search: search.clone(),
        list: list.clone(),
        stack: stack.clone(),
        banner: banner.clone(),
    };
    {
        let ui = ui.clone();
        let page = page.clone();
        search.connect_search_changed(move |_| {
            if let Some(model) = ui.model.borrow().as_ref().cloned() {
                page.refresh(&ui, &model);
            }
        });
    }
    (page, root)
}

impl Page {
    pub fn refresh(&self, ui: &Ui, model: &Model) {
        let query = self.search.text();
        let hits = model.search(&query, Scope::Installed);

        while let Some(child) = self.list.last_child() {
            self.list.remove(&child);
        }
        for entry in hits.iter().take(MAX_ROWS) {
            self.list.append(&make_row(ui, entry));
        }
        if hits.len() > MAX_ROWS {
            let more = gtk::Label::new(Some(&format!(
                "Showing the first {MAX_ROWS} of {} — keep typing to narrow.",
                hits.len()
            )));
            more.add_css_class("dim-label");
            more.set_margin_top(6);
            self.list.append(&more);
        }

        let updates = model.updatable().len();
        if updates > 0 {
            let noun = if updates == 1 { "update is" } else { "updates are" };
            self.banner.set_title(&format!("{updates} {noun} available"));
            self.banner.set_revealed(true);
        } else {
            self.banner.set_revealed(false);
        }

        if hits.is_empty() {
            let title = if query.is_empty() { "No installed packages" } else { "No matches" };
            if let Some(child) = self.stack.child_by_name("empty")
                && let Some(page) = child.downcast_ref::<adw::StatusPage>()
            {
                page.set_title(title);
            }
            self.stack.set_visible_child_name("empty");
        } else {
            self.stack.set_visible_child_name("list");
        }
    }
}

fn make_row(ui: &Ui, entry: &crate::model::Entry) -> adw::ActionRow {
    // On this page the version shown is what is installed; an update is
    // shown as "→ new" in the tag, not by silently replacing it.
    let version = entry
        .installed
        .as_ref()
        .map(|l| l.version.as_str())
        .unwrap_or_else(|| entry.display_version());
    // ActionRow title/subtitle are Pango markup: escape everything dynamic.
    let row = adw::ActionRow::builder()
        .title(markup_escape(&format!("{}  {}", entry.name, version)))
        .subtitle(markup_escape(&entry.description))
        .build();
    row.set_subtitle_lines(1);

    match entry.status() {
        Status::UpdateAvailable => {
            let sync = entry.sync.as_ref().expect("UpdateAvailable implies sync");
            let tag = gtk::Label::new(Some(&format!("→ {}", sync.version)));
            tag.add_css_class("dim-label");
            row.add_suffix(&tag);
            add_button(&row, "Update", "suggested-action", ui, entry.name.clone(), true, 0);
        }
        Status::InstalledExplicit => {
            let frees = entry.installed.as_ref().map(|l| l.isize).unwrap_or(0);
            add_button(&row, "Uninstall", "flat", ui, entry.name.clone(), false, frees);
        }
        Status::InstalledDep => {
            let tag = gtk::Label::new(Some("dependency"));
            tag.add_css_class("dim-label");
            row.add_suffix(&tag);
        }
        Status::Available => {}
    }
    row
}

/// The Uninstall / Update button. Uninstall confirms with the real impact
/// (dependents it would break, orphans `-Rns` would take); Update just
/// runs `pacman -S --needed` for that name.
fn add_button(
    row: &adw::ActionRow,
    label: &str,
    css: &str,
    ui: &Ui,
    name: String,
    is_update: bool,
    frees: u64,
) {
    let button = gtk::Button::builder().label(label).valign(gtk::Align::Center).build();
    button.add_css_class(css);
    {
        let ui = ui.clone();
        button.connect_clicked(move |_| {
            if is_update {
                ui.start_txn(Txn::Install(vec![name.clone()]));
                return;
            }
            let Some(window) = ui.window.borrow().clone() else { return };
            let (breaks, orphans) = {
                let model = match ui.model.borrow().as_ref() {
                    Some(m) => m.clone(),
                    None => return,
                };
                (model.reverse_deps(&name), model.orphans_after_removal(&name))
            };
            let mut details = Vec::new();
            if !breaks.is_empty() {
                details.push(format!(
                    "This breaks packages that depend on it: {}.",
                    breaks.join(", ")
                ));
            }
            if frees > 0 {
                details.push(format!("It frees {} of disk.", super::human_size(frees)));
            }
            if !orphans.is_empty() {
                let shown = orphans.iter().take(12).cloned().collect::<Vec<_>>().join(", ");
                let rest = if orphans.len() > 12 {
                    format!(", and {} more", orphans.len() - 12)
                } else {
                    String::new()
                };
                details.push(format!("It also removes unneeded dependencies: {shown}{rest}."));
            }
            details.push("The operation runs as root and its output is shown.".into());
            let ui2 = ui.clone();
            let name_for_txn = name.clone();
            dialog::confirm(
                &window,
                &format!("Remove {name}?"),
                &format!("{name} and its files will be removed from the system."),
                &details,
                "Remove",
                true,
                move || ui2.start_txn(Txn::Remove(vec![name_for_txn])),
            );
        });
    }
    row.add_suffix(&button);
}
