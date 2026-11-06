//! Shared dialog builders.

use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::i18n;
use crate::model;

/// A modal confirm with an explanation, optional detail lines, and one
/// action button. Every removal and every local-file install goes through
/// here — the confirmation the helper runs `--noconfirm` on.
pub fn confirm(
    parent: &adw::ApplicationWindow,
    title: &str,
    body: &str,
    details: &[String],
    button: &str,
    destructive: bool,
    on_confirm: impl FnOnce() + 'static,
) {
    let dialog = adw::Dialog::builder().title(title).content_width(480).build();

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_start(24);
    content.set_margin_end(24);
    content.set_margin_top(18);
    content.set_margin_bottom(18);

    let label = gtk::Label::new(Some(body));
    label.set_wrap(true);
    label.set_xalign(0.0);
    content.append(&label);

    for detail in details {
        let detail = gtk::Label::new(Some(detail));
        detail.set_wrap(true);
        detail.set_xalign(0.0);
        detail.add_css_class("dim-label");
        content.append(&detail);
    }

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    actions.set_margin_top(6);
    let cancel = gtk::Button::builder().label(i18n::t("action.cancel")).build();
    cancel.add_css_class("flat");
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.force_close());
    }
    let go = gtk::Button::builder().label(button).build();
    go.add_css_class(if destructive { "destructive-action" } else { "suggested-action" });
    {
        let dialog = dialog.clone();
        // connect_clicked wants an `Fn`; the confirmation runs once, so the
        // closure is taken out of the cell on the first press.
        let on_confirm = std::cell::RefCell::new(Some(on_confirm));
        go.connect_clicked(move |_| {
            if let Some(run) = on_confirm.borrow_mut().take() {
                run();
            }
            dialog.force_close();
        });
    }
    actions.append(&cancel);
    actions.append(&go);
    content.append(&actions);

    dialog.set_child(Some(&content));
    dialog.present(Some(parent));
}

/// One question per unanswered virtual, all in one dialog: a request whose
/// dependency is a virtual with several candidates is exactly where pacman
/// would silently take provider #1. The rows show `name — repository —
/// size` in pacman's own candidate order and the first is preselected —
/// the default the user is allowed to overrule. The concrete names chosen
/// here are what goes into the transaction, so pacman is never asked a
/// second time (`--noconfirm` would only answer with the same default).
pub fn choose_providers(
    parent: &adw::ApplicationWindow,
    questions: &[model::ProviderQuestion],
    on_confirm: impl FnOnce(Vec<String>) + 'static,
) {
    let dialog = adw::Dialog::builder()
        .title(i18n::t("chooser.title"))
        .content_width(520)
        // A virtual like `java-runtime` has ~14 candidates; the dialog
        // caps its height and the groups scroll, while the actions stay
        // pinned below.
        .content_height(520)
        .build();

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 12);

    let scrolled = gtk::ScrolledWindow::new();
    scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scrolled.set_vexpand(true);
    outer.append(&scrolled);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_start(24);
    content.set_margin_end(24);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    scrolled.set_child(Some(&content));

    let body = gtk::Label::new(Some(&i18n::t("chooser.body")));
    body.set_wrap(true);
    body.set_xalign(0.0);
    content.append(&body);

    // Each question's candidates are one radio group (grouped check
    // buttons — libadwaita 0.9 has no AdwRadioGroup); the pairs let the
    // confirm handler map a checked button back to a package name.
    let groups: std::cell::RefCell<Vec<Vec<(gtk::CheckButton, String)>>> =
        std::cell::RefCell::new(Vec::new());
    for question in questions {
        let heading = gtk::Label::new(Some(&question.virtual_name));
        heading.set_xalign(0.0);
        heading.add_css_class("heading");
        content.append(&heading);

        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        let mut rows = Vec::new();
        for choice in &question.candidates {
            let row = adw::ActionRow::builder()
                .title(&choice.name)
                .subtitle(format!(
                    "{} — {}",
                    choice.repository,
                    super::human_size(choice.csize)
                ))
                .build();
            let check = gtk::CheckButton::new();
            // Grouping on the first candidate of this question makes the
            // buttons exclusive without any manual toggling.
            check.set_group(rows.first().map(|(c, _)| c));
            row.set_activatable_widget(Some(&check));
            row.add_suffix(&check);
            row.set_activatable(true);
            list.append(&row);
            rows.push((check, choice.name.clone()));
        }
        // pacman's default is the first candidate; preselect it so the
        // dialog's initial state *is* the answer pacman would have given.
        if let Some((first, _)) = rows.first() {
            first.set_active(true);
        }
        content.append(&list);
        groups.borrow_mut().push(rows);
    }

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    actions.set_margin_top(6);
    let cancel = gtk::Button::builder().label(i18n::t("action.cancel")).build();
    cancel.add_css_class("flat");
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.force_close());
    }
    let go = gtk::Button::builder()
        .label(i18n::t("action.install"))
        .build();
    go.add_css_class("suggested-action");
    {
        let dialog = dialog.clone();
        // connect_clicked wants an `Fn`; the answer runs once, so the
        // closure is taken out of the cell on the first press.
        let on_confirm = std::cell::RefCell::new(Some(on_confirm));
        go.connect_clicked(move |_| {
            let chosen = groups
                .borrow()
                .iter()
                .map(|rows| {
                    rows.iter()
                        .find(|(check, _)| check.is_active())
                        .or_else(|| rows.first())
                        .map(|(_, name)| name.clone())
                        .expect("a question always has candidates")
                })
                .collect();
            if let Some(run) = on_confirm.borrow_mut().take() {
                run(chosen);
            }
            dialog.force_close();
        });
    }
    actions.append(&cancel);
    actions.append(&go);
    outer.append(&actions);

    dialog.set_child(Some(&outer));
    dialog.present(Some(parent));
}
