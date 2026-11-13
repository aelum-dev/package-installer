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
use crate::i18n;
use crate::model::{Model, Scope, Status, Unremovable};
use crate::txn::Txn;

#[derive(Clone)]
pub struct Page {
    search: gtk::SearchEntry,
    list: gtk::ListBox,
    stack: gtk::Stack,
}

pub fn build(ui: &Ui) -> (Page, gtk::Box) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(&i18n::t("installed.search")));
    search.set_margin_start(12);
    search.set_margin_end(12);
    search.set_margin_top(12);
    root.append(&search);

    // No update banner here: the Updates tab carries its own badge, and a
    // second count on this page would be the same fact said twice.
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    // Bottom margin only: it pads inside the frame at the end of the scroll;
    // the sides and bottom edge of the frame are the viewport's.
    list.set_margin_bottom(12);
    list.add_css_class("boxed-list");

    let empty = adw::StatusPage::builder()
        .icon_name("package-x-generic-symbolic")
        .title(i18n::t("status.reading"))
        .build();

    // A ListBox is not scrollable: its minimum height is the whole list, so
    // unscrolled rows grow the window instead of the scrollbar.
    let scrolled = gtk::ScrolledWindow::new();
    scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scrolled.set_vexpand(true);
    // The card frame wraps the viewport, not the content: its bottom line
    // stays at the window edge while the rows scroll inside it. The list
    // keeps .boxed-list for its row styling; window.rs strips the card it
    // would otherwise paint around the whole (scrolled) content.
    scrolled.add_css_class("card");
    scrolled.set_margin_start(12);
    scrolled.set_margin_end(12);
    scrolled.set_margin_bottom(12);
    scrolled.set_child(Some(&list));

    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    // Nothing sits between the entry and the list here — the gap between
    // a page's header row and its list card is this margin's job.
    stack.set_margin_top(12);
    stack.add_named(&scrolled, Some("list"));
    stack.add_named(&empty, Some("empty"));
    stack.set_visible_child_name("empty");
    root.append(&stack);

    let page = Page {
        search: search.clone(),
        list: list.clone(),
        stack: stack.clone(),
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
            self.list.append(&make_row(ui, model, entry));
        }
        if hits.len() > MAX_ROWS {
            let more = gtk::Label::new(Some(&i18n::t_fmt(
                "rows.truncated",
                &[
                    ("max", &MAX_ROWS.to_string()),
                    ("total", &hits.len().to_string()),
                ],
            )));
            more.add_css_class("dim-label");
            more.set_margin_top(6);
            self.list.append(&more);
        }

        if hits.is_empty() {
            let title = if query.is_empty() {
                i18n::t("installed.empty")
            } else {
                i18n::t("rows.noMatches")
            };
            if let Some(child) = self.stack.child_by_name("empty")
                && let Some(page) = child.downcast_ref::<adw::StatusPage>()
            {
                page.set_title(&title);
            }
            self.stack.set_visible_child_name("empty");
        } else {
            self.stack.set_visible_child_name("list");
        }
    }
}

fn make_row(ui: &Ui, model: &Model, entry: &crate::model::Entry) -> adw::ActionRow {
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
            let label = i18n::t("action.update");
            add_button(&row, &label, "suggested-action", ui, entry.name.clone(), true);
        }
        Status::InstalledExplicit => {
            let label = i18n::t("action.uninstall");
            let button = add_button(&row, &label, "flat", ui, entry.name.clone(), false);
            if let Some(reason) = model.unremovable(&entry.name) {
                // The row is listed and updated like any other; only the
                // removal is refused, and the grey has to say why. An
                // insensitive button takes no pointer events, so the sentence
                // goes on the row too — otherwise it reads as a bug rather
                // than as a choice.
                let why = unremovable_text(reason, &entry.name);
                button.set_sensitive(false);
                button.set_tooltip_text(Some(&why));
                row.set_tooltip_text(Some(&why));
            }
        }
        Status::InstalledDep => {
            let tag = gtk::Label::new(Some(&i18n::t("tag.dependency")));
            tag.add_css_class("dim-label");
            row.add_suffix(&tag);
        }
        Status::Available => {}
    }
    row
}

/// The sentence behind a greyed Remove: the model decides *that* a row cannot
/// be removed and which kind of reason that is, and only the words are the
/// page's, so the model carries no message strings. The name is interpolated
/// where the reason is about a role — "this system would not boot" is only
/// useful if it says what would stop it.
fn unremovable_text(reason: Unremovable, name: &str) -> String {
    match reason {
        Unremovable::SelfPackage => i18n::t("installed.selfCannotRemove"),
        Unremovable::PackageManager => i18n::t("installed.pacmanCannotRemove"),
        Unremovable::Boot => i18n::t_fmt("installed.bootCannotRemove", &[("name", name)]),
        Unremovable::Initramfs => {
            i18n::t_fmt("installed.initramfsCannotRemove", &[("name", name)])
        }
    }
}

/// The Uninstall / Update button. Uninstall confirms with the two facts that
/// can change the answer; Update just runs `pacman -S --needed` for that name.
/// The button is returned so a caller can make it insensitive.
fn add_button(
    row: &adw::ActionRow,
    label: &str,
    css: &str,
    ui: &Ui,
    name: String,
    is_update: bool,
) -> gtk::Button {
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
                // The one fact that decides the answer: pacman refuses to
                // remove a package other packages still need.
                details.push(i18n::t_fmt(
                    "dialog.remove.stillNeeded",
                    &[("list", &name_list(&breaks))],
                ));
            }
            if !orphans.is_empty() {
                // A count, not a list: the run's own log names every package
                // it takes, and this dialog is not the place to pre-print it.
                details.push(i18n::t_fmt(
                    "dialog.remove.orphans",
                    &[("count", &orphans.len().to_string())],
                ));
            }
            let ui2 = ui.clone();
            let name_for_txn = name.clone();
            dialog::confirm(
                &window,
                &i18n::t_fmt("dialog.remove.title", &[("name", &name)]),
                // The question is asked in words: a bare fact with a button
                // next to it does not read as a confirmation.
                &i18n::t("dialog.remove.body"),
                &details,
                &i18n::t("action.remove"),
                true,
                move || ui2.start_txn(Txn::Remove(vec![name_for_txn])),
            );
        });
    }
    row.add_suffix(&button);
    button
}

/// Names for a dialog line: all of them when there are few, a count with a
/// sample when there are many — a wall of names is what makes a confirmation
/// unreadable.
fn name_list(names: &[String]) -> String {
    const SHOWN: usize = 3;
    if names.len() <= SHOWN {
        return names.join(", ");
    }
    i18n::t_fmt(
        "dialog.nameList",
        &[
            ("count", &names.len().to_string()),
            ("sample", &names.iter().take(SHOWN).cloned().collect::<Vec<_>>().join(", ")),
        ],
    )
}
