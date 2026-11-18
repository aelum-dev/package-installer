//! The Firmware page: what fwupd says about the devices on this machine.
//!
//! Like the AUR page, this page asks a third party's question and therefore
//! asks it **only when pressed**: no badge, no startup query. LVFS is someone
//! else's server, and "2 firmware updates waiting" would be a claim about a
//! server the user never contacted — the same offline-first rule that keeps
//! the AUR tab silent. The press runs `fwupdmgr refresh` first (that is what
//! makes the answer today's), then reads the update list; a refresh that
//! fails is said, not hidden, because the list it falls back to is the
//! metadata already on the machine.
//!
//! The rows are fwupd's own answer, kept as it came: a device with a release
//! is an update to write, a device waiting on a restart says so, and a failed
//! device carries fwupd's own sentence untranslated — the way a pacman error
//! sits in a log. The Update press writes **all** of them (`fwupdmgr update`,
//! the same thing the same press on the Updates page is for packages); the
//! daemon decides device by device, and an id on the command line could only
//! mean a write this confirmation never named.
//!
//! The Check press is the page's one constant act, so it is one button: it
//! sits centered below the status text while the page has no rows — the
//! answer area and its act in one place — and moves to the header row beside
//! Update only when rows are on screen.

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;

use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use super::app::Ui;
use super::dialog;
use super::human_size;
use super::markup_escape;
use crate::fwupd;
use crate::i18n;
use crate::txn::Txn;

#[derive(Clone)]
pub struct Page {
    check: gtk::Button,
    update: gtk::Button,
    /// The header row: count, Update, and Check while rows are up.
    header: gtk::Box,
    list: gtk::ListBox,
    stack: gtk::Stack,
    status: adw::StatusPage,
    /// The status page's child in every rowless state: the spinner (while
    /// checking) above the centered Check button (below all the text).
    state_box: gtk::Box,
    spinner: gtk::Spinner,
    count: gtk::Label,
    /// The one line the list carries above its rows: a restart is owed, or
    /// the metadata this answer used is stale.
    note: gtk::Label,
    /// Which of the two boxes currently holds Check (see `park_check`): the
    /// header row (true) or the state box (false). Build puts it in the
    /// state box, and every state transition moves it only if needed.
    check_in_header: Rc<Cell<bool>>,
}

pub fn build(ui: &Ui) -> (Page, gtk::Box) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // Count on the left, acts on the right — the Updates page's shape:
    // this page's number is "what waits", and the button does about it.
    // Check joins this row only while rows are up (see `Page::move_check_into`).
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header.set_margin_start(12);
    header.set_margin_end(12);
    header.set_margin_top(12);
    let count = gtk::Label::builder().xalign(0.0).build();
    count.set_hexpand(true);
    let update = gtk::Button::builder().label(i18n::t("action.update")).build();
    update.add_css_class("suggested-action");
    update.set_valign(gtk::Align::Center);
    update.set_visible(false);
    let check = gtk::Button::builder().label(i18n::t("firmware.check")).build();
    check.set_halign(gtk::Align::Center);
    check.set_tooltip_text(Some(&i18n::t("firmware.checkTip")));
    header.append(&count);
    header.append(&update);
    root.append(&header);

    let note = gtk::Label::builder().xalign(0.0).build();
    note.add_css_class("dim-label");
    note.add_css_class("caption");
    note.set_wrap(true);
    note.set_margin_start(12);
    note.set_margin_end(12);
    note.set_margin_top(6);
    note.set_visible(false);
    root.append(&note);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_margin_bottom(12);
    list.add_css_class("boxed-list");

    let spinner = gtk::Spinner::new();
    spinner.set_size_request(32, 32);
    spinner.set_spinning(false);
    spinner.set_visible(false);
    let status = adw::StatusPage::builder()
        .icon_name("media-flash-symbolic")
        .title(i18n::t("firmware.idle"))
        .description(i18n::t("firmware.idleBody"))
        .build();

    // The rowless states' answer area: whatever the page has to say, then
    // the press that asks, centered below it all. StatusPage centers its
    // child under the title and description.
    let state_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    state_box.append(&spinner);
    state_box.append(&check);
    status.set_child(Some(&state_box));

    let scrolled = gtk::ScrolledWindow::new();
    scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scrolled.set_vexpand(true);
    scrolled.add_css_class("card");
    scrolled.set_margin_start(12);
    scrolled.set_margin_end(12);
    scrolled.set_margin_bottom(12);
    scrolled.set_child(Some(&list));

    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    stack.set_margin_top(12);
    stack.add_named(&scrolled, Some("list"));
    stack.add_named(&status, Some("empty"));
    stack.set_visible_child_name("empty");
    root.append(&stack);

    let page = Page {
        check: check.clone(),
        update: update.clone(),
        header,
        list,
        stack,
        status,
        state_box,
        spinner,
        count,
        note,
        check_in_header: Rc::new(Cell::new(false)),
    };
    {
        let ui = ui.clone();
        check.connect_clicked(move |_| ui.check_firmware());
    }
    {
        // The write is the one act here that touches hardware, so it is the
        // one press that answers first: how many devices, which names, and
        // that the machine may reboot on its own when a write finishes.
        let ui = ui.clone();
        update.connect_clicked(move |_| {
            let Some((count, names)) = pending(&ui) else { return };
            let body = if count == 1 {
                i18n::t_fmt("firmware.updateBodyOne", &[("name", &names)])
            } else {
                i18n::t_fmt(
                    "firmware.updateBodyMany",
                    &[("count", &count.to_string()), ("names", &names)],
                )
            };
            let ui2 = ui.clone();
            dialog::confirm(
                &ui,
                &i18n::t("firmware.updateTitle"),
                &body,
                &[i18n::t("firmware.updateNote")],
                &i18n::t("action.update"),
                false,
                move || ui2.update_firmware(),
            );
        });
    }
    (page, root)
}

/// The page's current write: how many devices the last answer listed, and
/// their names — three at most, then the fact that there are more, the way
/// a removal confirmation names what still depends. None with nothing
/// pending: the button cannot be pressed then, and if it somehow is, the
/// confirmation of an empty request is not worth a dialog.
fn pending(ui: &Ui) -> Option<(usize, String)> {
    let stored = ui.fw_answer.borrow();
    let devices: Vec<&fwupd::Device> = stored
        .as_ref()?
        .as_ref()
        .ok()
        .map(|answer| answer.updates())
        .unwrap_or_default();
    if devices.is_empty() {
        return None;
    }
    let mut names = devices
        .iter()
        .take(3)
        .map(|d| d.name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    if devices.len() > 3 {
        names.push_str(" …");
    }
    Some((devices.len(), names))
}

impl Page {
    /// The one Check button, parked in one of its two homes: the state box
    /// (centered below the status text) while the page is rowless, the
    /// header row beside Update while rows are up. A widget has one parent,
    /// so moving it is how a page keeps one button for one act. The flag is
    /// the memory of which box holds it — the build starts in the state box
    /// and every transition moves it at most once, so `remove` always finds
    /// its child.
    fn park_check(&self, in_header: bool) {
        if self.check_in_header.get() == in_header {
            return;
        }
        let (from, to): (&gtk::Box, &gtk::Box) = if in_header {
            (&self.state_box, &self.header)
        } else {
            (&self.header, &self.state_box)
        };
        from.remove(&self.check);
        to.append(&self.check);
        self.check_in_header.set(in_header);
    }

    /// Every rowless state: the status page owns the area, and its child is
    /// the spinner-plus-Check box centered below the text.
    fn show_status(&self) {
        self.spinner.set_spinning(false);
        self.spinner.set_visible(false);
        self.park_check(false);
        self.status.set_child(Some(&self.state_box));
        self.stack.set_visible_child_name("empty");
    }

    pub fn refresh(&self, ui: &Ui) {
        while let Some(child) = self.list.last_child() {
            self.list.remove(&child);
        }
        // The presence of the client is a filesystem fact, read fresh: the
        // page's own Install press puts fwupd on the system, and the model
        // reload that follows repaints this page into the checked state
        // without any extra signalling.
        let installed = Path::new(fwupd::BINARY).exists();
        self.check.set_sensitive(installed && !ui.fw_checking.get());
        self.update.set_visible(false);
        self.count.set_visible(false);
        self.note.set_visible(false);

        if !installed {
            self.status.set_icon_name(Some("media-flash-symbolic"));
            self.status.set_title(&i18n::t("firmware.none"));
            self.status.set_description(Some(&i18n::t("firmware.noneBody")));
            let button = gtk::Button::builder()
                .label(i18n::t("firmware.install"))
                .halign(gtk::Align::Center)
                .build();
            button.add_css_class("suggested-action");
            let ui = ui.clone();
            button.connect_clicked(move |_| {
                // The package that puts the daemon on the system goes
                // through the same transaction flow as every other
                // install — the confirmation, the log, all of it.
                let ui2 = ui.clone();
                dialog::confirm(
                    &ui,
                    &i18n::t("firmware.none"),
                    &i18n::t("firmware.noneBody"),
                    &[],
                    &i18n::t("firmware.install"),
                    false,
                    move || {
                        ui2.start_txn(Txn::Install(vec![fwupd::PACKAGE.to_string()]));
                    },
                );
            });
            // The Check button belongs to a page that cannot check yet: it
            // rests in the detached state box rather than waiting in the
            // header row, greyed and toasting.
            self.park_check(false);
            self.status.set_child(Some(&button));
            self.stack.set_visible_child_name("empty");
            return;
        }

        if ui.fw_checking.get() {
            self.status.set_icon_name(None);
            self.status.set_title(&i18n::t("firmware.checking"));
            self.status.set_description(None::<&str>);
            self.spinner.set_visible(true);
            self.spinner.set_spinning(true);
            self.park_check(false);
            self.status.set_child(Some(&self.state_box));
            self.stack.set_visible_child_name("empty");
            return;
        }

        let stored = ui.fw_answer.borrow();
        let Some(result) = stored.as_ref() else {
            // Pressed nothing yet: the page says what the press would ask.
            self.status.set_icon_name(Some("media-flash-symbolic"));
            self.status.set_title(&i18n::t("firmware.idle"));
            self.status.set_description(Some(&i18n::t("firmware.idleBody")));
            self.show_status();
            return;
        };
        let Ok(answer) = result.as_ref() else {
            // The check itself failed — fwupd's own sentence is the
            // description, like a pacman error in a log. An empty list is
            // never standing in for this.
            self.status.set_icon_name(Some("network-error-symbolic"));
            self.status.set_title(&i18n::t("firmware.failed"));
            self.status
                .set_description(result.as_ref().err().map(String::as_str));
            self.show_status();
            return;
        };

        let updates = answer.updates();
        if updates.is_empty() && !answer.needs_reboot() {
            // fwupd listed nothing: every supported device is current.
            self.status.set_icon_name(Some("object-select-symbolic"));
            self.status.set_title(&i18n::t("firmware.upToDate"));
            let stale = i18n::t("firmware.refreshFailed");
            self.status.set_description(if answer.refresh_failed {
                Some(&stale)
            } else {
                None
            });
            self.show_status();
            return;
        }
        if answer.devices.is_empty() {
            // A restart owed with no device in the answer: the daemon's
            // flag says one thing and its list nothing, so the page says
            // the flag — an empty list never stands in for a state.
            self.status.set_icon_name(Some("system-reboot-symbolic"));
            self.status.set_title(&i18n::t("firmware.rebootNeeded"));
            self.status.set_description(None::<&str>);
            self.show_status();
            return;
        }

        for device in &answer.devices {
            self.list.append(&make_row(device));
        }
        if !updates.is_empty() {
            let count = updates.len().to_string();
            self.count.set_text(&i18n::t_fmt(
                if updates.len() == 1 {
                    "firmware.nUpdatesOne"
                } else {
                    "firmware.nUpdatesMany"
                },
                &[("count", &count)],
            ));
            self.count.set_visible(true);
            self.update.set_visible(true);
        }
        // One note above the rows: the restart waits on work already done —
        // more actionable than the stale-metadata caveat it shares a slot
        // with, which still reads on the empty states.
        if answer.needs_reboot() {
            self.note.set_text(&i18n::t("firmware.rebootNeeded"));
            self.note.set_visible(true);
        } else if answer.refresh_failed {
            self.note.set_text(&i18n::t("firmware.refreshFailed"));
            self.note.set_visible(true);
        }
        // Rows are up: Check takes its place in the header row, beside the
        // Update it feeds — both acts are next to what they work on.
        self.status.set_child(None::<&gtk::Widget>);
        self.park_check(true);
        self.stack.set_visible_child_name("list");
    }
}

/// One device the check answered about. An update row reads the way an
/// Updates row does — installed version, arrow, new version, size; every
/// other row is one sentence about what fwupd already did with it.
fn make_row(device: &fwupd::Device) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(markup_escape(&device.name))
        .build();
    if let Some(rel) = device.update.as_ref() {
        let current = device.version.as_deref().unwrap_or("?");
        row.set_subtitle(&format!("{} → {}", current, rel.version));
        row.set_subtitle_lines(1);
        if let Some(size) = rel.size {
            let bytes = gtk::Label::new(Some(&human_size(size)));
            bytes.add_css_class("dim-label");
            row.add_suffix(&bytes);
        }
        return row;
    }
    let sentence = if device.state.needs_reboot() {
        i18n::t("firmware.rowRestart")
    } else if device.error.is_some() {
        device
            .error
            .clone()
            .unwrap_or_else(|| i18n::t("firmware.rowFailed"))
    } else {
        i18n::t("firmware.rowCurrent")
    };
    row.set_subtitle(&markup_escape(&sentence));
    row.set_subtitle_lines(1);
    row
}
