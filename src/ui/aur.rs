//! The AUR page: the Arch User Repository, searched when the user asks.
//!
//! The AUR is not a repository — it is a archive of PKGBUILDs — so this page
//! is not Browse over a different list. A search brings recipes; installing
//! one builds it on this machine. Two rules keep that honest:
//!
//! - **Nothing is fetched unasked.** There is no badge on this tab and no
//!   startup query. This desktop is offline-first, and "3 AUR updates waiting"
//!   would be a claim about a third-party server the user never contacted.
//! - **An empty list is never a silent failure.** The AUR answers a query
//!   that matches too much (`arg=python`) with HTTP 200, an empty array and
//!   the sentence `Too many package results.` — the page shows that sentence.
//!
//! The rows are classified against the model the moment the answer arrives
//! (`Model::aur_rows`), so a row says what that name is *on this system*:
//! not installed, installed, or installed and older than what the AUR would
//! build.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use super::app::{MAX_ROWS, Ui};
use super::dialog;
use super::markup_escape;
use crate::i18n;
use crate::model::{AurRow, AurState};

/// Keystrokes settle before a request goes out. The AUR rate-limits, and a
/// search for one word typed five ways is five searches.
const DEBOUNCE: Duration = Duration::from_millis(400);

#[derive(Clone)]
pub struct Page {
    search: gtk::SearchEntry,
    list: gtk::ListBox,
    stack: gtk::Stack,
    status: adw::StatusPage,
    spinner: gtk::Spinner,
    /// The debounce timer: one pending search per page, replaced on every
    /// keystroke.
    pending: Rc<RefCell<Option<glib::SourceId>>>,
}

pub fn build(ui: &Ui) -> (Page, gtk::Box) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(&i18n::t("aur.search")));
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

    let spinner = gtk::Spinner::new();
    spinner.set_size_request(32, 32);
    let status = adw::StatusPage::builder()
        .icon_name("system-search-symbolic")
        .title(i18n::t("aur.idle"))
        .description(i18n::t("aur.idleBody"))
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
    // Nothing sits between the entry and the list here — the row Browse's
    // freshness label gives, this margin has to.
    stack.set_margin_top(12);
    stack.add_named(&scrolled, Some("list"));
    stack.add_named(&status, Some("empty"));
    stack.set_visible_child_name("empty");
    root.append(&stack);

    let page = Page {
        search: search.clone(),
        list,
        stack,
        status,
        spinner,
        pending: Rc::new(RefCell::new(None)),
    };
    {
        let ui = ui.clone();
        let page = page.clone();
        let entry = search.clone();
        search.connect_search_changed(move |_| {
            // The entry's own clear button lands here with an empty query,
            // which is also how the page gets back to its starting state.
            page.recall(&ui, &entry.text());
        });
    }
    {
        // Enter is the "now" version of typing: no waiting for the timer.
        let ui = ui.clone();
        let pending = Rc::clone(&page.pending);
        search.connect_activate(move |entry| {
            if let Some(id) = pending.borrow_mut().take() {
                id.remove();
            }
            ui.search_aur(&entry.text());
        });
    }
    (page, root)
}

impl Page {
    /// Replace the pending search with this one.
    fn recall(&self, ui: &Ui, query: &str) {
        // A one-shot timeout is gone the moment it runs, and glib panics
        // ("Source ID … was not found") when asked to remove an id that
        // already ran — which aborted the app on the third keystroke. So the
        // callback drops the stored id before doing its work, and this only
        // ever removes one that is still waiting.
        if let Some(id) = self.pending.borrow_mut().take() {
            id.remove();
        }
        let query = query.to_string();
        if query.trim().is_empty() {
            // Clearing the box is an answer, not a question: no timer, and
            // the page goes back to what it says before anything was asked.
            ui.search_aur("");
            return;
        }
        let ui = ui.clone();
        let pending = Rc::clone(&self.pending);
        let id = glib::timeout_add_local(DEBOUNCE, move || {
            *pending.borrow_mut() = None;
            ui.search_aur(&query);
            glib::ControlFlow::Break
        });
        *self.pending.borrow_mut() = Some(id);
    }

    pub fn refresh(&self, ui: &Ui) {
        while let Some(child) = self.list.last_child() {
            self.list.remove(&child);
        }
        let searching = ui.aur_searching.get();
        let stored = ui.aur.borrow();
        let hits = stored.as_ref().map(|r| r.hits.as_slice()).unwrap_or(&[]);
        // Rows are built here, against the model as it is now: the same
        // answer is a different row after an install, and no rows at all is
        // the honest state while the databases are still being read.
        let model = ui.model.borrow();
        let rows: Vec<AurRow> = match model.as_ref() {
            Some(model) => model.aur_rows(hits),
            None => Vec::new(),
        };
        let reading = !hits.is_empty() && rows.is_empty();
        for row in rows.iter().take(MAX_ROWS) {
            self.list.append(&make_row(ui, row));
        }
        if rows.len() > MAX_ROWS {
            let more = gtk::Label::new(Some(&i18n::t_fmt(
                "rows.truncatedShort",
                &[
                    ("max", &MAX_ROWS.to_string()),
                    ("total", &rows.len().to_string()),
                ],
            )));
            more.add_css_class("dim-label");
            more.set_margin_top(6);
            self.list.append(&more);
        }

        if !rows.is_empty() && !searching {
            self.status.set_child(None::<&gtk::Widget>);
            self.stack.set_visible_child_name("list");
            return;
        }

        // Nothing to show. Which of the five that is, the page says.
        let error = stored.as_ref().and_then(|r| r.error.as_deref());
        if reading {
            self.spinner.set_spinning(true);
            self.status.set_child(Some(&self.spinner));
            self.status.set_title(&i18n::t("status.reading"));
            self.status.set_description(None::<&str>);
            self.stack.set_visible_child_name("empty");
            return;
        }
        if searching {
            self.status.set_icon_name(None);
            self.status.set_title(&i18n::t("aur.searching"));
            self.status.set_description(None::<&str>);
            self.status.set_child(Some(&self.spinner));
            self.spinner.set_spinning(true);
        } else if let Some(error) = error {
            self.spinner.set_spinning(false);
            self.status.set_child(None::<&gtk::Widget>);
            self.status.set_icon_name(Some("network-error-symbolic"));
            self.status.set_title(&i18n::t("aur.failed"));
            self.status.set_description(Some(error));
        } else if self.search.text().is_empty() {
            self.spinner.set_spinning(false);
            self.status.set_child(None::<&gtk::Widget>);
            self.status.set_icon_name(Some("system-search-symbolic"));
            self.status.set_title(&i18n::t("aur.idle"));
            self.status.set_description(Some(&i18n::t("aur.idleBody")));
        } else {
            self.spinner.set_spinning(false);
            self.status.set_child(None::<&gtk::Widget>);
            self.status.set_icon_name(Some("system-search-symbolic"));
            self.status.set_title(&i18n::t("aur.empty"));
            self.status.set_description(None::<&str>);
        }
        self.stack.set_visible_child_name("empty");
    }
}

/// One AUR result. The title carries the versions the same way the Updates
/// page does — an AUR update is `installed → to build`, because there is no
/// binary to download.
fn make_row(ui: &Ui, row: &AurRow) -> adw::ActionRow {
    let hit = &row.hit;
    let title = match row.state {
        AurState::Update => format!(
            "{}  {} → {}",
            hit.name,
            row.installed_version.as_deref().unwrap_or(""),
            hit.version
        ),
        _ => format!("{}  {}", hit.name, hit.version),
    };
    let row_widget = adw::ActionRow::builder()
        .title(markup_escape(&title))
        .subtitle(markup_escape(&hit.description))
        .build();
    row_widget.set_subtitle_lines(1);

    // The vote count is the AUR's own signal about a package nobody has
    // tested for you, and the number the AUR's site shows next to the name.
    let votes = gtk::Label::new(Some(&format!("▲ {}", hit.votes)));
    votes.add_css_class("dim-label");
    row_widget.add_suffix(&votes);

    if hit.out_of_date {
        // The AUR itself flags this: the upstream has moved and the PKGBUILD
        // has not. It is worth saying before someone starts a build.
        let tag = gtk::Label::new(Some(&i18n::t("aur.outOfDate")));
        tag.add_css_class("dim-label");
        row_widget.add_suffix(&tag);
    }

    match row.state {
        AurState::NotInstalled | AurState::Update => {
            let updating = row.state == AurState::Update;
            let (title_key, button) = if updating {
                ("dialog.update.title", i18n::t("action.update"))
            } else {
                ("dialog.install.title", i18n::t("action.install"))
            };
            let widget = gtk::Button::builder()
                .label(&button)
                .valign(gtk::Align::Center)
                .build();
            widget.add_css_class("suggested-action");
            let ui = ui.clone();
            let hit = hit.clone();
            widget.connect_clicked(move |_| {
                let Some(window) = ui.window.borrow().clone() else { return };
                // The sentence the sync rows never need: nothing here is
                // downloaded ready-made.
                let body = i18n::t_fmt(
                    "dialog.aur.body",
                    &[("name", &hit.name), ("version", &hit.version)],
                );
                let mut details = vec![i18n::t("aur.buildNote")];
                if !hit.url.is_empty() {
                    details.push(hit.url.clone());
                }
                let ui2 = ui.clone();
                let hit2 = hit.clone();
                dialog::confirm(
                    &window,
                    &i18n::t_fmt(title_key, &[("name", &hit.name)]),
                    &body,
                    &details,
                    &button,
                    false,
                    move || ui2.install_aur(hit2),
                );
            });
            row_widget.add_suffix(&widget);
        }
        AurState::Installed => {
            let tag = gtk::Label::new(Some(&i18n::t("tag.installed")));
            tag.add_css_class("dim-label");
            row_widget.add_suffix(&tag);
        }
    }
    row_widget
}
