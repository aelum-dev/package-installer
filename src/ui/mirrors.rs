//! The mirror-update dialog: the question, the country picker, the spinner.
//!
//! One question — sync the mirrors for which country? — over a list of the
//! mirror countries (the `countries` table, fuzzy-filtered by a search entry),
//! preselected to the country the system's own files point at. Pressing
//! Update switches the dialog to its spinner and runs the job through the
//! one polkit action: install reflector first when the host has none (it is
//! not part of an Aelum install — the dialog says so above the buttons before
//! the press, never after), then the helper's `update-mirrors` verb. Lines
//! stream into a buffer, not a txn log: a mirror rewrite has no pacman phases
//! to paint, and the spinner is the progress the request asked for. Only a
//! failure reopens the form — with reflector's own words, so nothing here
//! fails silently.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use futures::StreamExt;
use gtk::glib;
use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use super::app::Ui;
use crate::countries;
use crate::i18n;
use crate::model::MIRRORLIST;
use crate::txn::{self, Txn, TxnEvent};

/// Where reflector lives; when it is absent, Update puts it there first —
/// through the same one polkit action every other install goes through.
const REFLECTOR: &str = "/usr/bin/reflector";

/// The system locale — the installer's own record, and the country default's
/// last source after the mirrorlist.
const LOCALE_CONF: &str = "/etc/locale.conf";

/// The dialog's two faces: the question, and the spinner that replaces it
/// while the job runs.
const FORM: &str = "form";
const BUSY: &str = "busy";

/// Open the dialog. Only the manager's menu reaches it, so the toast lane
/// exists; the running gate answers here at the press, not at the finish —
/// two mirror updates, or a mirror update over a transaction, are both races
/// with pacman's own lock and none of the app's business to win.
pub fn show(ui: &Ui) {
    if ui.running.get() {
        ui.toast(&i18n::t("toast.busy"));
        return;
    }
    if !Path::new(txn::HELPER).exists() {
        ui.toast(&i18n::t_fmt("toast.noHelper", &[("helper", txn::HELPER)]));
        return;
    }

    // The country this system's own files point at. Two small reads, and
    // only because someone opened the dialog.
    let selected = countries::default_index(
        std::fs::read_to_string(MIRRORLIST).ok().as_deref(),
        locale_conf_text().as_deref(),
    );
    let selected = Rc::new(RefCell::new(selected));

    let dialog = adw::Dialog::builder()
        .title(i18n::t("mirrors.title"))
        .content_width(460)
        .content_height(520)
        .build();
    let stack = gtk::Stack::new();

    // ── The form: the question, the note, the picker, the acts. ──
    let form = gtk::Box::new(gtk::Orientation::Vertical, 12);
    form.set_margin_start(24);
    form.set_margin_end(24);
    form.set_margin_top(18);
    form.set_margin_bottom(18);

    let body = gtk::Label::new(Some(&i18n::t("mirrors.question")));
    body.set_wrap(true);
    body.set_xalign(0.0);
    form.append(&body);

    // The dialog never installs anything nobody mentioned: the host that has
    // no reflector is told, over the buttons, before the press that does it.
    let note = gtk::Label::new(Some(&i18n::t("mirrors.willInstall")));
    note.set_wrap(true);
    note.set_xalign(0.0);
    note.add_css_class("dim-label");
    note.set_visible(!Path::new(REFLECTOR).exists());
    form.append(&note);

    // The country picker: a search entry over the list — the pages' shape,
    // because AdwComboRow's built-in search cannot do better than substring
    // and its custom match hook is not bound in the libadwaita 0.9 crate.
    // The list is rebuilt from the pure `countries::search` on every
    // keystroke; ≤ 74 rows, so no cap and no debounce (only the AUR debounces,
    // and that is a rate-limited server).
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(&i18n::t("mirrors.search")));
    form.append(&search);

    let scrolled = gtk::ScrolledWindow::new();
    scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scrolled.set_vexpand(true);
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    scrolled.set_child(Some(&list));
    form.append(&scrolled);

    {
        let list = list.clone();
        let selected = Rc::clone(&selected);
        let search = search.clone();
        search.connect_search_changed(move |entry| {
            fill_countries(&list, &entry.text(), &selected);
        });
    }
    fill_countries(&list, &search.text(), &selected);

    // A failure's words, kept between the question and the buttons: the form
    // comes back with them, so a refused update reads as one retryable fact
    // and not a silent reset.
    let failure = gtk::Label::new(None);
    failure.set_wrap(true);
    failure.set_xalign(0.0);
    failure.set_selectable(true);
    failure.add_css_class("error-label");
    failure.set_visible(false);
    form.append(&failure);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::builder()
        .label(i18n::t("action.cancel"))
        .build();
    cancel.add_css_class("flat");
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| {
            dialog.close();
        });
    }
    let update = gtk::Button::builder()
        .label(i18n::t("action.update"))
        .build();
    update.add_css_class("suggested-action");
    {
        let ui = ui.clone();
        let dialog = dialog.clone();
        let stack = stack.clone();
        let failure = failure.clone();
        let note = note.clone();
        let selected = Rc::clone(&selected);
        update.connect_clicked(move |_| {
            if ui.running.get() {
                ui.toast(&i18n::t("toast.busy"));
                return;
            }
            // The index the picker last confirmed. Worldwide answers with no
            // country — every mirror, still rate-sorted — which is exactly
            // the installer's worldwide location.
            let country = countries::COUNTRIES[*selected.borrow()].code;
            let country = (country != countries::WORLDWIDE_CODE).then(|| country.to_string());
            // reflector is the mechanism the dialog offers; a host without it
            // gets it as the job's first step — the same install every other
            // package takes, visible above before the press.
            let mut steps = Vec::new();
            if !Path::new(REFLECTOR).exists() {
                steps.push(Txn::Install(vec!["reflector".into()]));
            }
            steps.push(Txn::UpdateMirrors(country));

            ui.running.set(true);
            // The run outlives this dialog if the window does: closing the
            // manager would otherwise quit the app mid-transaction (its last
            // window rule), and the root half keeps working regardless. This
            // token — dropped when the answer lands or the stream dies — is
            // what makes closing honest.
            let hold = ui.app.borrow().as_ref().map(|app| app.hold());
            failure.set_visible(false);
            stack.set_visible_child_name(BUSY);

            let (tx, mut rx) = futures::channel::mpsc::unbounded();
            let spawned = txn::run_all(&steps, tx);
            let ui = ui.clone();
            // The press closure is an `Fn`: the async task takes its own
            // handles of everything it outlives a press by.
            let dialog = dialog.clone();
            let stack = stack.clone();
            let failure = failure.clone();
            let note = note.clone();
            glib::MainContext::default().spawn_local(async move {
                let _hold = hold; // released when the answer lands or the stream dies
                let mut lines: Vec<String> = Vec::new();
                let outcome: Result<(), String> = match spawned {
                    Err(err) => Err(i18n::t_fmt(
                        "toast.startFailed",
                        &[("error", &err.to_string())],
                    )),
                    Ok(()) => loop {
                        match rx.next().await {
                            Some(TxnEvent::Line(line)) => lines.push(line),
                            Some(TxnEvent::Step(_)) => {}
                            Some(TxnEvent::Finished(Ok(()))) => break Ok(()),
                            Some(TxnEvent::Finished(Err(_))) => break Err(failure_text(&lines)),
                            // The worker died without a status — the answer
                            // keeps the same shape, with whatever lines made
                            // it that far (which may be none, and still be
                            // honest).
                            None => break Err(failure_text(&lines)),
                        }
                    },
                };
                ui.running.set(false);
                match outcome {
                    Ok(()) => {
                        // The pages say nothing new — mirror lists are not
                        // packages — so the one toast is the whole answer.
                        dialog.close();
                        ui.toast(&i18n::t("mirrors.done"));
                    }
                    Err(detail) => {
                        // Back to the question, with the reason between the
                        // picker and the buttons: retry or cancel, eyes open.
                        note.set_visible(!Path::new(REFLECTOR).exists());
                        failure.set_text(&detail);
                        failure.set_visible(true);
                        stack.set_visible_child_name(FORM);
                    }
                }
            });
        });
    }
    actions.append(&cancel);
    actions.append(&update);
    form.append(&actions);

    // ── The spinner: the whole content while the job runs. ──
    let busy = gtk::Box::new(gtk::Orientation::Vertical, 12);
    busy.set_valign(gtk::Align::Center);
    busy.set_halign(gtk::Align::Center);
    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    spinner.set_size_request(32, 32);
    busy.append(&spinner);
    let waiting = gtk::Label::new(Some(&i18n::t("mirrors.running")));
    waiting.set_wrap(true);
    waiting.set_justify(gtk::Justification::Center);
    busy.append(&waiting);

    stack.add_named(&form, Some(FORM));
    stack.add_named(&busy, Some(BUSY));
    stack.set_visible_child_name(FORM);
    dialog.set_child(Some(&stack));
    ui.present_dialog(&dialog);
}

/// (Re)build the picker for one query. The selection lives in `selected`, so
/// a keystroke filters the rows without losing the country behind them.
fn fill_countries(list: &gtk::ListBox, query: &str, selected: &Rc<RefCell<usize>>) {
    list.remove_all();
    let want = *selected.borrow();
    let mut first: Option<gtk::CheckButton> = None;
    for (index, country) in countries::search(query) {
        let row = adw::ActionRow::builder().title(country.name).build();
        if country.code != countries::WORLDWIDE_CODE {
            // The code the request will carry, so what a "us" typed into the
            // search found is visible beside its name.
            row.set_subtitle(country.code);
        }
        let check = gtk::CheckButton::new();
        // One radio group for the whole list (libadwaita 0.9 has no
        // AdwRadioGroup — the provider chooser's trick).
        check.set_group(first.as_ref());
        if first.is_none() {
            first = Some(check.clone());
        }
        check.set_active(index == want);
        // A row is its check: click anywhere on it to choose. The grouped
        // buttons turn the previous one off by themselves; each stores its
        // own index the moment it is the active one.
        row.set_activatable_widget(Some(&check));
        row.add_suffix(&check);
        row.set_activatable(true);
        {
            let selected = Rc::clone(selected);
            check.connect_toggled(move |check| {
                if check.is_active() {
                    *selected.borrow_mut() = index;
                }
            });
        }
        list.append(&row);
    }
}

/// The system locale text: the installer's file, or — a source tree has none
/// — the same answer from the environment.
fn locale_conf_text() -> Option<String> {
    std::fs::read_to_string(LOCALE_CONF).ok().or_else(|| {
        std::env::var("LANG")
            .ok()
            .map(|value| format!("LANG={value}"))
    })
}

/// A failure has to say something true: the job's own words, last lines
/// first (reflector and pacman end with their errors), capped so the dialog
/// stays a dialog.
fn failure_text(lines: &[String]) -> String {
    const SHOWN: usize = 6;
    let head = i18n::t("mirrors.failed");
    let tail: Vec<&str> = lines
        .iter()
        .rev()
        .take(SHOWN)
        .rev()
        .map(String::as_str)
        .collect();
    if tail.is_empty() {
        head
    } else {
        format!("{head}\n{}", tail.join("\n"))
    }
}
