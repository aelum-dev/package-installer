//! The Updates page: every installed package whose repository carries a
//! newer version. The startup check fills it, and the ViewStackPage's
//! needs-attention marks the tab when it is not empty.

use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use super::app::{MAX_ROWS, Ui};
use super::human_size;
use super::markup_escape;
use crate::model::{Entry, Model};
use crate::txn::Txn;

#[derive(Clone)]
pub struct Page {
    list: gtk::ListBox,
    stack: gtk::Stack,
    header: gtk::Box,
    count: gtk::Label,
}

pub fn build(ui: &Ui) -> (Page, gtk::Box) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header.set_margin_start(12);
    header.set_margin_end(12);
    header.set_margin_top(12);
    let count = gtk::Label::builder().xalign(0.0).build();
    count.set_hexpand(true);
    let all = gtk::Button::builder().label("Update All").build();
    all.add_css_class("suggested-action");
    {
        let ui = ui.clone();
        all.connect_clicked(move |_| ui.start_txn(Txn::Upgrade));
    }
    header.append(&count);
    header.append(&all);
    // GTK4 shows children by default; the count row appears with the
    // first update, never above an empty state.
    header.set_visible(false);
    root.append(&header);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_margin_start(12);
    list.set_margin_end(12);
    list.set_margin_bottom(12);
    list.add_css_class("boxed-list");

    let empty = adw::StatusPage::builder()
        .icon_name("object-select-symbolic")
        .title("Reading package databases…")
        .build();

    // A ListBox is not scrollable: its minimum height is the whole list, so
    // unscrolled rows grow the window instead of the scrollbar.
    let scrolled = gtk::ScrolledWindow::new();
    scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scrolled.set_vexpand(true);
    scrolled.set_child(Some(&list));

    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    stack.add_named(&scrolled, Some("list"));
    stack.add_named(&empty, Some("empty"));
    stack.set_visible_child_name("empty");
    root.append(&stack);

    (
        Page {
            list,
            stack,
            header,
            count,
        },
        root,
    )
}

impl Page {
    pub fn refresh(&self, ui: &Ui, model: &Model) {
        let hits = model.updatable();

        while let Some(child) = self.list.last_child() {
            self.list.remove(&child);
        }
        for entry in hits.iter().take(MAX_ROWS) {
            self.list.append(&make_row(ui, entry));
        }
        if hits.len() > MAX_ROWS {
            let more = gtk::Label::new(Some(&format!(
                "Showing the first {MAX_ROWS} of {}.",
                hits.len()
            )));
            more.add_css_class("dim-label");
            more.set_margin_top(6);
            self.list.append(&more);
        }

        if hits.is_empty() {
            self.header.set_visible(false);
            if let Some(child) = self.stack.child_by_name("empty")
                && let Some(page) = child.downcast_ref::<adw::StatusPage>()
            {
                // "Up to date" is only a fact when the databases were
                // readable; an unreadable one would otherwise hide behind it.
                if model.db_errors.is_empty() {
                    page.set_title("Everything up to date");
                    page.set_description(None::<&str>);
                } else {
                    page.set_title("Package databases could not be read");
                    page.set_description(Some(&model.db_errors.join("; ")));
                }
            }
            self.stack.set_visible_child_name("empty");
        } else {
            let noun = if hits.len() == 1 { "update is" } else { "updates are" };
            self.count.set_text(&format!("{} {} available.", hits.len(), noun));
            self.header.set_visible(true);
            self.stack.set_visible_child_name("list");
        }
    }
}

fn make_row(ui: &Ui, entry: &Entry) -> adw::ActionRow {
    // updatable() only returns UpdateAvailable entries, so both versions
    // are here. ActionRow titles are Pango markup: escape everything
    // dynamic.
    let installed = entry
        .installed
        .as_ref()
        .map(|l| l.version.as_str())
        .unwrap_or("");
    let new = entry
        .sync
        .as_ref()
        .map(|s| s.version.as_str())
        .unwrap_or("");
    let row = adw::ActionRow::builder()
        .title(markup_escape(&format!("{}  {} → {}", entry.name, installed, new)))
        .subtitle(markup_escape(&entry.description))
        .build();
    row.set_subtitle_lines(1);

    if let Some(repo) = &entry.repository {
        let tag = gtk::Label::new(Some(&format!("[{repo}]")));
        tag.add_css_class("dim-label");
        row.add_suffix(&tag);
    }
    if let Some(sync) = &entry.sync && sync.csize > 0 {
        let size = gtk::Label::new(Some(&format!("↓ {}", human_size(sync.csize))));
        size.add_css_class("dim-label");
        row.add_suffix(&size);
    }

    // A single-package update needs no confirmation: the row itself shows
    // the old and new version, the repo, and the download size — the same
    // contract as the Update button on the Installed page. The log dialog
    // owns the rest.
    let button = gtk::Button::builder().label("Update").valign(gtk::Align::Center).build();
    button.add_css_class("suggested-action");
    let ui = ui.clone();
    let name = entry.name.clone();
    button.connect_clicked(move |_| ui.start_txn(Txn::Install(vec![name.clone()])));
    row.add_suffix(&button);
    row
}
