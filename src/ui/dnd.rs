//! Drag & drop: dropping a package file anywhere on the window opens the
//! same confirm-and-install flow as opening it from a file manager. The
//! target type is `gdk::FileList` — GTK's own representation of
//! `text/uri-list` — so a file-manager drag (one file or many) arrives as
//! a list of GFiles, and non-file drags never reach the handlers: the
//! target rejects them by format.
//!
//! Advertise COPY *and* MOVE: what a source offers is its own choice —
//! Nautilus drags here as MOVE-only, and a COPY-only target fails the
//! action negotiation before the drop signal ever fires (the silent
//! "nothing happens" first attempt, learned in the video-player). `enter`
//! must answer with a single action from the intersection; the default
//! handler computes exactly that, so mirror it rather than hard-coding
//! COPY.

use gtk::prelude::*;

use super::app::Ui;

/// Attach the window's drop target. `drop_hint` is the "Drop to install"
/// overlay, shown while a file drag is inside the window.
pub(super) fn attach(window: &impl IsA<gtk::Widget>, drop_hint: &gtk::Label, ui: &Ui) {
    let drop = gtk::DropTarget::new(
        gtk::gdk::FileList::static_type(),
        gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE,
    );
    {
        let refreshing = ui.refreshing.clone();
        drop.connect_accept(move |target, drop| {
            // While a database refresh covers the pages, dropping is not
            // available: refusing at accept means the drag never even
            // raises its hint, and the page that replaced the tabs is the
            // sentence that says why.
            if refreshing.get() {
                return false;
            }
            // accept's accumulator is first-wins: this handler answers
            // instead of the class default, so it owes the default's two
            // checks too — offered actions ∩ ours, and the drop's formats
            // convertible to the target's type.
            if (drop.actions() & target.actions()).is_empty() {
                return false;
            }
            match target.formats() {
                Some(formats) => {
                    formats.match_type(&drop.formats()) != gtk::glib::types::Type::INVALID
                }
                None => true,
            }
        });
        let hint = drop_hint.clone();
        drop.connect_enter(move |target, _x, _y| {
            hint.set_visible(true);
            let offered = target
                .current_drop()
                .map(|d| d.actions())
                .unwrap_or(gtk::gdk::DragAction::COPY);
            for action in [
                gtk::gdk::DragAction::COPY,
                gtk::gdk::DragAction::MOVE,
                gtk::gdk::DragAction::LINK,
            ] {
                if offered.contains(action) {
                    return action;
                }
            }
            gtk::gdk::DragAction::MOVE
        });
        let hint = drop_hint.clone();
        drop.connect_leave(move |_| {
            hint.set_visible(false);
        });
        let hint = drop_hint.clone();
        let ui = ui.clone();
        drop.connect_drop(move |_target, value, _x, _y| {
            // `leave` is not guaranteed to follow a drop, so hide the hint here.
            hint.set_visible(false);
            // A refresh can still have started mid-drag (the startup check
            // fires on the first model load, and a drag that entered before
            // it was accepted then): refuse, don't promise the busy toast.
            if ui.refreshing.get() {
                return false;
            }
            let Ok(list) = value.get_owned::<gtk::gdk::FileList>() else {
                return false;
            };
            ui.open_files(&list.files());
            true
        });
    }
    window.add_controller(drop);
}
