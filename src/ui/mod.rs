//! The GTK shell: shared state (`app`), the window, the five pages, and
//! the dialogs. UI state lives on the GTK main thread; only the model loading
//! and the transaction output cross threads (channels into this thread).

mod app;
mod aur;
mod browse;
mod dialog;
mod distro;
mod fileopen;
mod installed;
mod txnlog;
mod updates;
pub(crate) mod window;

pub use app::Ui;

/// ActionRow titles/subtitles are Pango markup, and package descriptions
/// carry bare `&` (firefox's does) — glib dropped `escape_text`, so the
/// three markup characters get escaped here.
pub(crate) fn markup_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Sizes the way pacman's own dialogs phrase them.
pub(crate) fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
