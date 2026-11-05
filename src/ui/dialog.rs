//! Shared dialog builders.

use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

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
    let cancel = gtk::Button::builder().label("Cancel").build();
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
