//! The AUR page: the Arch User Repository, searched when the user asks.
//!
//! The AUR is not a repository — it is a archive of PKGBUILDs — so this page
//! is not Browse over a different list. A search brings recipes; installing
//! one builds it on this machine. Three rules keep that honest:
//!
//! - **Nothing is fetched unasked.** There is no badge on this tab and no
//!   startup query. This desktop is offline-first, and "3 AUR updates waiting"
//!   would be a claim about a third-party server the user never contacted.
//! - **The update check is asked for too.** Its button asks the AUR about the
//!   names no repository carries (`Model::foreign_names` — pacman's `-Qqm`)
//!   and keeps only the rows with a newer version. A press, not a badge, and
//!   its "nothing newer" answer states how many names it checked: that count
//!   is the only thing that makes the silence a fact.
//! - **An empty list is never a silent failure.** The AUR answers a query
//!   that matches too much (`arg=python`) with HTTP 200, an empty array and
//!   the sentence `Too many package results.` — the page shows that sentence.
//!
//! The rows are classified against the model the moment the answer arrives
//! (`Model::aur_rows`, and `Model::aur_updates` for a check), so a row says
//! what that name is *on this system*: not installed, installed, or installed
//! and older than what the AUR would build.

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
    /// The update check: a question about the packages this system installed
    /// outside its repositories, asked only when the user presses it.
    check: gtk::Button,
    list: gtk::ListBox,
    stack: gtk::Stack,
    status: adw::StatusPage,
    spinner: gtk::Spinner,
    idle_warn: gtk::Label,
    /// The debounce timer: one pending search per page, replaced on every
    /// keystroke.
    pending: Rc<RefCell<Option<glib::SourceId>>>,
}

pub fn build(ui: &Ui) -> (Page, gtk::Box) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // The entry and the check share one row: both are questions this page
    // can ask, and the page has one answer area for either.
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header.set_margin_start(12);
    header.set_margin_end(12);
    header.set_margin_top(12);
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(&i18n::t("aur.search")));
    search.set_hexpand(true);
    let check = gtk::Button::builder().label(i18n::t("aur.check")).build();
    check.set_valign(gtk::Align::Center);
    check.set_tooltip_text(Some(&i18n::t("aur.checkTip")));
    header.append(&search);
    header.append(&check);
    root.append(&header);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    // Bottom margin only: it pads inside the frame at the end of the scroll;
    // the sides and bottom edge of the frame are the viewport's.
    list.set_margin_bottom(12);
    list.add_css_class("boxed-list");

    let spinner = gtk::Spinner::new();
    spinner.set_size_request(32, 32);
    // The idle state says what the page is (no search yet, nothing fetched
    // unasked) and, below that, what kind of place the AUR is: community-
    // published, unreviewed, built here. The rows' vote counts and
    // out-of-date tags are the practical follow-through.
    let idle_warn = gtk::Label::new(Some(&i18n::t("aur.warn")));
    idle_warn.set_wrap(true);
    idle_warn.set_justify(gtk::Justification::Center);
    idle_warn.set_max_width_chars(48);
    idle_warn.set_margin_top(18);
    idle_warn.set_margin_start(24);
    idle_warn.set_margin_end(24);
    let status = adw::StatusPage::builder()
        .icon_name("system-search-symbolic")
        .title(i18n::t("aur.idle"))
        .description(i18n::t("aur.idleBody"))
        .build();
    status.set_child(Some(&idle_warn));

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
    stack.add_named(&status, Some("empty"));
    stack.set_visible_child_name("empty");
    root.append(&stack);

    let page = Page {
        search: search.clone(),
        check: check.clone(),
        list,
        stack,
        status,
        spinner,
        idle_warn,
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
    {
        // A check is a different question than the text in the entry, so the
        // entry goes empty first: the page returns to the state a check owns
        // and its answer is never read as the search's. Clearing the entry
        // also drops a search that was still waiting for the debounce.
        let ui = ui.clone();
        let entry = search.clone();
        check.connect_clicked(move |_| {
            entry.set_text("");
            ui.check_aur_updates();
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
        // One request at a time: a second press while one is in flight could
        // only have its answer thrown away by the sequence guard.
        self.check.set_sensitive(!searching);
        let stored = ui.aur.borrow();
        let hits = stored.as_ref().map(|r| r.hits.as_slice()).unwrap_or(&[]);
        let checking = stored.as_ref().is_some_and(|r| r.updates_only);
        let checked = stored.as_ref().and_then(|r| r.checked);
        // Rows are built here, against the model as it is now: the same
        // answer is a different row after an install, and no rows at all is
        // the honest state while the databases are still being read.
        let model = ui.model.borrow();
        let rows: Vec<AurRow> = match model.as_ref() {
            // A check keeps only the names the AUR would build newer than
            // what is installed. Listing the ones it asked about that are up
            // to date would read as a list of problems.
            Some(model) if checking => model.aur_updates(hits),
            Some(model) => model.aur_rows(hits),
            None => Vec::new(),
        };
        // An unread model is the only reason an answer yields no rows — a
        // check that found nothing newer answers with no rows on purpose.
        let reading = model.is_none() && !hits.is_empty();
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

        // Nothing to show. Which of the seven that is, the page says.
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
            self.status.set_title(&i18n::t(if checking {
                "aur.checking"
            } else {
                "aur.searching"
            }));
            self.status.set_description(None::<&str>);
            self.status.set_child(Some(&self.spinner));
            self.spinner.set_spinning(true);
        } else if let Some(error) = error {
            self.spinner.set_spinning(false);
            self.status.set_child(None::<&gtk::Widget>);
            self.status.set_icon_name(Some("network-error-symbolic"));
            self.status.set_title(&i18n::t(if checking {
                "aur.checkFailed"
            } else {
                "aur.failed"
            }));
            self.status.set_description(Some(error));
        } else if checking {
            // A check with no rows: either there was nothing to ask about, or
            // every name asked about is already current. The count is what
            // makes the second one a fact rather than a silence.
            self.spinner.set_spinning(false);
            self.status.set_child(None::<&gtk::Widget>);
            let asked = checked.unwrap_or(0);
            if asked == 0 {
                self.status.set_icon_name(Some("package-x-generic-symbolic"));
                self.status.set_title(&i18n::t("aur.checkNone"));
                self.status.set_description(Some(&i18n::t("aur.checkNoneBody")));
            } else {
                let count = asked.to_string();
                let key = if asked == 1 {
                    "aur.checkUpToDateOne"
                } else {
                    "aur.checkUpToDateMany"
                };
                self.status.set_icon_name(Some("object-select-symbolic"));
                self.status.set_title(&i18n::t("aur.checkUpToDate"));
                self.status.set_description(Some(&i18n::t_fmt(key, &[("count", &count)])));
            }
        } else if self.search.text().is_empty() {
            self.spinner.set_spinning(false);
            self.status.set_icon_name(Some("system-search-symbolic"));
            self.status.set_title(&i18n::t("aur.idle"));
            self.status.set_description(Some(&i18n::t("aur.idleBody")));
            // The search note first, the trust warning below it.
            self.status.set_child(Some(&self.idle_warn));
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
    // The count alone reads as a bare glyph, so the tooltip says what it is;
    // the star is one Pango size step up so it reads as an icon, the count
    // staying at the row's font. Markup, but only a glyph and an integer.
    let votes = gtk::Label::new(None);
    votes.set_markup(&format!("<span size=\"large\">★</span> {}", hit.votes));
    votes.add_css_class("dim-label");
    votes.set_tooltip_text(Some(&i18n::t_fmt(
        "aur.votesTip",
        &[("votes", &hit.votes.to_string())],
    )));
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
