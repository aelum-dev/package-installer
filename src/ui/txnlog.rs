//! The transaction log: pacman's own output, streamed while it runs and
//! kept on screen when it fails. A failed operation that shows nothing is
//! the bug this whole file exists to prevent.

use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::i18n;

/// Handles into the log dialog — cheap to clone, shared between the event
/// receiver and the failure path.
#[derive(Clone)]
pub struct TxnLog {
    buffer: gtk::TextBuffer,
    scrolled: gtk::ScrolledWindow,
    spinner: gtk::Spinner,
    status: gtk::Label,
    close: gtk::Button,
}

pub fn show(window: &adw::ApplicationWindow, summary: &str) -> TxnLog {
    let dialog = adw::Dialog::builder()
        .title(i18n::t("txnlog.title"))
        .content_width(640)
        .content_height(420)
        .build();

    let content = gtk::Box::new(gtk::Orientation::Vertical, 10);

    let summary_label = gtk::Label::new(Some(summary));
    summary_label.set_xalign(0.0);
    summary_label.set_margin_start(16);
    summary_label.set_margin_end(16);
    summary_label.set_margin_top(12);
    content.append(&summary_label);

    let view = gtk::TextView::new();
    view.set_monospace(true);
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    let scrolled = gtk::ScrolledWindow::new();
    scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scrolled.set_vexpand(true);
    scrolled.set_child(Some(&view));
    content.append(&scrolled);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_margin_start(16);
    actions.set_margin_end(16);
    actions.set_margin_bottom(12);
    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    let status = gtk::Label::new(Some(&i18n::t("txnlog.running")));
    status.add_css_class("dim-label");
    let close = gtk::Button::builder().label(i18n::t("action.close")).build();
    close.set_sensitive(false);
    {
        let dialog = dialog.clone();
        close.connect_clicked(move |_| dialog.force_close());
    }
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    actions.append(&spinner);
    actions.append(&status);
    actions.append(&spacer);
    actions.append(&close);
    content.append(&actions);

    dialog.set_child(Some(&content));
    dialog.present(Some(window));

    TxnLog {
        buffer: view.buffer(),
        scrolled,
        spinner,
        status,
        close,
    }
}

impl TxnLog {
    pub fn append(&self, line: &str) {
        let mut end = self.buffer.end_iter();
        self.buffer.insert(&mut end, &format!("{line}\n"));
        // Follow the output: the interesting part is always the tail.
        let adj = self.scrolled.vadjustment();
        adj.set_value(adj.upper());
    }

    pub fn finish(&self, result: Result<(), i32>) {
        self.spinner.set_spinning(false);
        match result {
            Ok(()) => self.status.set_text(&i18n::t("txnlog.done")),
            // pkexec: 127 = the action was refused, 1 = the auth prompt was
            // dismissed. pacman's own failure codes pass through.
            Err(code) => {
                self.status
                    .set_text(&i18n::t_fmt("txnlog.failed", &[("code", &code.to_string())]));
            }
        }
        self.close.set_sensitive(true);
    }
}
