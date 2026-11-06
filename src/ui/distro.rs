//! The Aelum page: the distro's own apps — everything the `[aelum]`
//! repository carries that this system does not have yet. Publishing an app
//! to the channel puts it here; installing one moves it to the Installed
//! page, which is the distribution story told inside the app.

use gtk::prelude::*;
use libadwaita as adw;

use super::app::{MAX_ROWS, Ui};
use super::browse;
use crate::i18n;
use crate::model::Model;

#[derive(Clone)]
pub struct Page {
    list: gtk::ListBox,
    stack: gtk::Stack,
}

pub fn build() -> (Page, gtk::Box) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    // Bottom margin only: it pads inside the frame at the end of the scroll;
    // the frame's own margins are the viewport's.
    list.set_margin_bottom(12);
    list.add_css_class("boxed-list");

    let empty = adw::StatusPage::builder()
        .icon_name("starred-symbolic")
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
    scrolled.set_margin_top(12);
    scrolled.set_margin_bottom(12);
    scrolled.set_child(Some(&list));

    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    stack.add_named(&scrolled, Some("list"));
    stack.add_named(&empty, Some("empty"));
    stack.set_visible_child_name("empty");
    root.append(&stack);

    (Page { list, stack }, root)
}

impl Page {
    pub fn refresh(&self, ui: &Ui, model: &Model) {
        let hits = model.distro_apps();

        while let Some(child) = self.list.last_child() {
            self.list.remove(&child);
        }
        for entry in hits.iter().take(MAX_ROWS) {
            self.list.append(&browse::make_row(ui, entry));
        }
        if hits.len() > MAX_ROWS {
            let more = gtk::Label::new(Some(&i18n::t_fmt(
                "rows.truncatedShort",
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
            if let Some(child) = self.stack.child_by_name("empty")
                && let Some(page) = child.downcast_ref::<adw::StatusPage>()
            {
                // An empty list here has three different meanings, and the
                // page says which one it is — "nothing to add" is only a
                // fact when the [aelum] database was actually readable.
                let db_error: Vec<&str> = model
                    .db_errors
                    .iter()
                    .filter(|e| e.starts_with(crate::model::DISTRO_REPO))
                    .map(|e| e.as_str())
                    .collect();
                if !db_error.is_empty() {
                    page.set_title(&i18n::t("aelum.dbError"));
                    page.set_description(Some(&db_error.join("; ")));
                } else if model.has_distro_repo() {
                    page.set_title(&i18n::t("aelum.empty"));
                    page.set_description(None::<&str>);
                } else {
                    page.set_title(&i18n::t("aelum.noRepo"));
                    page.set_description(Some(&i18n::t("aelum.noRepoBody")));
                }
            }
            self.stack.set_visible_child_name("empty");
        } else {
            self.stack.set_visible_child_name("list");
        }
    }
}
