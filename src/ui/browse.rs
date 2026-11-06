//! The Browse page: every package in the configured sync repositories,
//! searched client-side. The repo tag makes `[aelum]` visible next to the
//! official repos — this page is where the distribution channel shows up.

use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use super::app::{MAX_ROWS, Ui};
use super::dialog;
use super::human_size;
use super::markup_escape;
use crate::db;
use crate::i18n;
use crate::model::{Model, Scope, Status};
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
    search.set_placeholder_text(Some(&i18n::t("browse.search")));
    search.set_margin_start(12);
    search.set_margin_end(12);
    search.set_margin_top(12);
    root.append(&search);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    // Bottom margin only: it pads inside the frame at the end of the scroll;
    // the sides and bottom edge of the frame are the viewport's.
    list.set_margin_bottom(12);
    list.add_css_class("boxed-list");

    let empty = adw::StatusPage::builder()
        .icon_name("system-search-symbolic")
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
    // Nothing sits between the entry and the list here — the gap the
    // freshness row used to give, this margin has to.
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
        let hits = model.search(&query, Scope::All);

        while let Some(child) = self.list.last_child() {
            self.list.remove(&child);
        }
        for entry in hits.iter().take(MAX_ROWS) {
            self.list.append(&make_row(ui, entry));
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
                i18n::t("browse.empty")
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

/// The shared row builder: Browse and the Aelum page draw the same
/// Available/Update row, confirmation included.
pub(crate) fn make_row(ui: &Ui, entry: &crate::model::Entry) -> adw::ActionRow {
    // ActionRow title/subtitle are Pango markup: escape everything
    // dynamic.
    let row = adw::ActionRow::builder()
        .title(markup_escape(&format!("{}  {}", entry.name, entry.display_version())))
        .subtitle(markup_escape(&entry.description))
        .build();
    row.set_subtitle_lines(1);

    if let Some(repo) = &entry.repository {
        let tag = gtk::Label::new(Some(&format!("[{repo}]")));
        tag.add_css_class("dim-label");
        row.add_suffix(&tag);
    }

    match entry.status() {
        Status::Available | Status::UpdateAvailable => {
            let updating = entry.status() == Status::UpdateAvailable;
            let label = if updating {
                i18n::t("action.update")
            } else {
                i18n::t("action.install")
            };
            let button = gtk::Button::builder()
                .label(&label)
                .valign(gtk::Align::Center)
                .build();
            button.add_css_class("suggested-action");
            let ui = ui.clone();
            let name = entry.name.clone();
            let new_version = entry.display_version().to_string();
            let (csize, isize_, depends, url) = entry
                .sync
                .as_ref()
                .map(|s| (s.csize, s.isize, s.depends.clone(), s.url.clone()))
                .unwrap_or_default();
            button.connect_clicked(move |_| {
                let Some(window) = ui.window.borrow().clone() else { return };
                // The question is asked in words, like the removal dialog:
                // a bare fact with a button next to it does not read as a
                // confirmation. The version rides in the question because
                // it is the thing being agreed to.
                let (title_key, body_key, button_label) = if updating {
                    (
                        "dialog.update.title",
                        "dialog.update.body",
                        i18n::t("action.update"),
                    )
                } else {
                    (
                        "dialog.install.title",
                        "dialog.install.body",
                        i18n::t("action.install"),
                    )
                };
                let body = i18n::t_fmt(
                    body_key,
                    &[("name", &name), ("version", &new_version)],
                );
                let mut details = Vec::new();
                if !url.is_empty() {
                    details.push(url.clone());
                }
                if csize > 0 {
                    details.push(i18n::t_fmt(
                        "dialog.download",
                        &[("size", &human_size(csize))],
                    ));
                }
                if isize_ > 0 {
                    details.push(i18n::t_fmt(
                        "dialog.installedSize",
                        &[("size", &human_size(isize_))],
                    ));
                }
                if let Some(model) = ui.model.borrow().as_ref().cloned() {
                    let missing: Vec<&str> = depends
                        .iter()
                        .map(|d| db::dep_name(d))
                        .filter(|d| {
                            model.get(d).and_then(|e| e.installed.as_ref()).is_none()
                        })
                        .collect();
                    if !missing.is_empty() {
                        // Two keys, not a Rust plural: "1 package" and the
                        // count form are different sentences in most of the
                        // 28 locales.
                        let also = if missing.len() == 1 {
                            i18n::t("dialog.alsoInstallsOne")
                        } else {
                            i18n::t_fmt(
                                "dialog.alsoInstallsMany",
                                &[("count", &missing.len().to_string())],
                            )
                        };
                        details.push(also);
                    }
                }
                let ui2 = ui.clone();
                let name2 = name.clone();
                dialog::confirm(
                    &window,
                    &i18n::t_fmt(title_key, &[("name", &name)]),
                    &body,
                    &details,
                    &button_label,
                    false,
                    // `pacman -S --needed` is both verbs: a fresh install
                    // or the one newer version the model found.
                    move || ui2.start_txn(Txn::Install(vec![name2])),
                );
            });
            row.add_suffix(&button);
        }
        Status::InstalledExplicit => {
            let tag = gtk::Label::new(Some(&i18n::t("tag.installed")));
            tag.add_css_class("dim-label");
            row.add_suffix(&tag);
        }
        Status::InstalledDep => {
            let tag = gtk::Label::new(Some(&i18n::t("tag.installedDep")));
            tag.add_css_class("dim-label");
            row.add_suffix(&tag);
        }
    }
    row
}

