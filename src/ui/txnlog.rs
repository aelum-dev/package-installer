//! The transaction dialog: a progress bar over the phases pacman reports,
//! with its own output behind a disclosure — streamed while it runs and
//! kept on screen when it fails. A failed operation that shows nothing is
//! the bug this whole file exists to prevent; a bar that lies about a
//! number it does not have is the bug `progress::Track` exists to prevent.
//!
//! The dialog is thin on purpose: it decides nothing. `Track` reads the
//! lines and answers phase/fraction/counts; this file only paints those
//! answers and buffers the text. Line appends and bar updates are
//! coalesced into one ~80 ms tick so a chatty makepkg cannot drive the
//! main thread.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::i18n;
use crate::progress::{self, Phase, StepKind, Track};

/// Lines kept in the view. makepkg output runs to thousands of lines; the
/// interesting part is always the tail, so the buffer drops from the
/// front past the cap.
const MAX_LOG_LINES: i32 = 2000;

/// Handles into the dialog — cheap to clone, shared between the event
/// receiver, the ticker, and the failure path.
#[derive(Clone)]
pub struct TxnLog {
    buffer: gtk::TextBuffer,
    scrolled: gtk::ScrolledWindow,
    spinner: gtk::Spinner,
    status: gtk::Label,
    counter: gtk::Label,
    bar: gtk::ProgressBar,
    disclosure: gtk::ToggleButton,
    close: gtk::Button,
    track: Rc<RefCell<Track>>,
    pending: Rc<RefCell<Vec<String>>>,
    done: Rc<Cell<bool>>,
}

pub fn show(window: &adw::ApplicationWindow, summary: &str, verb: progress::Verb) -> TxnLog {
    let dialog = adw::Dialog::builder()
        .title(i18n::t("txnlog.title"))
        .content_width(640)
        .content_height(420)
        .build();

    let content = gtk::Box::new(gtk::Orientation::Vertical, 10);

    let summary_label = gtk::Label::new(Some(summary));
    summary_label.set_xalign(0.0);
    summary_label.set_wrap(true);
    summary_label.set_margin_start(16);
    summary_label.set_margin_end(16);
    summary_label.set_margin_top(12);
    content.append(&summary_label);

    let status_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    status_row.set_margin_start(16);
    status_row.set_margin_end(16);
    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    let status = gtk::Label::new(Some(&i18n::t("txnlog.running")));
    let counter = gtk::Label::new(None);
    counter.add_css_class("dim-label");
    counter.set_visible(false);
    status_row.append(&spinner);
    status_row.append(&status);
    status_row.append(&counter);
    content.append(&status_row);

    let bar = gtk::ProgressBar::new();
    bar.set_show_text(false); // the phase line above the bar already says where it is
    bar.set_margin_start(16);
    bar.set_margin_end(16);
    content.append(&bar);

    // The output stays behind a disclosure: the bar and its phase line
    // answer "how far is it", and the log answers "what exactly". A
    // failure opens it by itself — nothing may fail silently.
    let disclosure = gtk::ToggleButton::builder()
        .label(i18n::t("txnlog.showOutput"))
        .active(false)
        .build();
    disclosure.set_halign(gtk::Align::Start);
    disclosure.set_margin_start(16);
    content.append(&disclosure);

    let view = gtk::TextView::new();
    view.set_monospace(true);
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    let scrolled = gtk::ScrolledWindow::new();
    scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scrolled.set_vexpand(true);
    scrolled.set_child(Some(&view));
    scrolled.set_visible(false);
    content.append(&scrolled);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_margin_start(16);
    actions.set_margin_end(16);
    actions.set_margin_bottom(12);
    let close = gtk::Button::builder().label(i18n::t("action.close")).build();
    close.set_sensitive(false);
    {
        let dialog = dialog.clone();
        close.connect_clicked(move |_| dialog.force_close());
    }
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    actions.append(&spacer);
    actions.append(&close);
    content.append(&actions);

    dialog.set_child(Some(&content));
    dialog.present(Some(window));

    let log = TxnLog {
        buffer: view.buffer(),
        scrolled: scrolled.clone(),
        spinner,
        status,
        counter,
        bar,
        disclosure: disclosure.clone(),
        close,
        track: Rc::new(RefCell::new(Track::new(verb))),
        pending: Rc::new(RefCell::new(Vec::new())),
        done: Rc::new(Cell::new(false)),
    };

    disclosure.connect_toggled(move |button| {
        let open = button.is_active();
        scrolled.set_visible(open);
        button.set_label(&i18n::t(if open {
            "txnlog.hideOutput"
        } else {
            "txnlog.showOutput"
        }));
    });

    // One paint per ~80 ms, however many lines arrive. The source id is
    // dropped inside the callback when it returns Break — `remove` on a
    // source that already finished panics, and a panic in a signal
    // trampoline aborts the process.
    let ticker = log.clone();
    let source_id: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let id_slot = source_id.clone();
    let source = glib::timeout_add_local(Duration::from_millis(80), move || {
        if ticker.done.get() {
            *id_slot.borrow_mut() = None;
            return glib::ControlFlow::Break;
        }
        ticker.flush();
        ticker.paint();
        glib::ControlFlow::Continue
    });
    *source_id.borrow_mut() = Some(source);

    log
}

impl TxnLog {
    /// One output line: `Track` reads it for the bar, the text waits for
    /// the next tick.
    pub fn append(&self, line: &str) {
        self.track.borrow_mut().feed(line);
        self.pending.borrow_mut().push(line.to_string());
    }

    /// A named step of a multi-step job (the AUR flow) began.
    pub fn step(&self, kind: StepKind) {
        self.track.borrow_mut().step(kind);
        self.paint();
    }

    pub fn finish(&self, result: Result<(), i32>) {
        self.done.set(true);
        self.flush();
        self.spinner.set_spinning(false);
        match result {
            Ok(()) => {
                self.bar.set_fraction(1.0);
                self.status.set_text(&i18n::t("txnlog.done"));
                self.counter.set_visible(false);
            }
            // pkexec: 127 = the action was refused, 1 = the auth prompt was
            // dismissed. pacman's own failure codes pass through.
            Err(code) => {
                self.status
                    .set_text(&i18n::t_fmt("txnlog.failed", &[("code", &code.to_string())]));
                // A failure shows its own evidence: open the disclosure.
                self.disclosure.set_active(true);
            }
        }
        self.close.set_sensitive(true);
    }

    fn flush(&self) {
        let lines = std::mem::take(&mut *self.pending.borrow_mut());
        if lines.is_empty() {
            return;
        }
        for line in lines {
            let mut end = self.buffer.end_iter();
            self.buffer.insert(&mut end, &format!("{line}\n"));
        }
        // Keep the tail: past the cap the oldest lines go.
        let excess = self.buffer.line_count() - MAX_LOG_LINES;
        if excess > 0
            && let (Some(mut start), Some(mut stop)) = (
                self.buffer.iter_at_line(0),
                self.buffer.iter_at_line(excess),
            )
        {
            self.buffer.delete(&mut start, &mut stop);
        }
        // Follow the output: the interesting part is always the tail.
        let adj = self.scrolled.vadjustment();
        adj.set_value(adj.upper());
    }

    /// Paint what `Track` currently believes. The label names the phase;
    /// a count appears only when one is honest (`Track::counts`).
    fn paint(&self) {
        let track = self.track.borrow();
        self.bar.set_fraction(track.fraction());
        self.status.set_text(&i18n::t(phase_key(track.phase())));
        match track.counts() {
            Some((done, total)) => {
                self.counter
                    .set_text(&i18n::t_fmt("txn.count", &[
                        ("done", &done.to_string()),
                        ("total", &total.to_string()),
                    ]));
                self.counter.set_visible(true);
            }
            None => self.counter.set_visible(false),
        }
    }
}

fn phase_key(phase: Phase) -> &'static str {
    match phase {
        Phase::Sync => "phase.sync",
        Phase::Resolve => "phase.resolve",
        Phase::Retrieve => "phase.retrieve",
        Phase::Check => "phase.check",
        Phase::Process => "phase.process",
        Phase::Hooks => "phase.hooks",
        Phase::Plan => "phase.plan",
        Phase::FetchDeps => "phase.fetchDeps",
        Phase::Build => "phase.build",
        Phase::InstallBuilt => "phase.installBuilt",
        Phase::Cleanup => "phase.cleanup",
        // Unknown: the transaction has not reported a phase yet; Done is
        // only reached when the work is over, and finish() owns that text.
        Phase::Unknown | Phase::Done => "txnlog.running",
    }
}
