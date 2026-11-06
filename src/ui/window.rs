//! The window: header, view switcher, and the five pages.

use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use super::app::Ui;
use super::{aur, browse, distro, installed, updates};
use crate::i18n;
use crate::txn::Txn;

/// Mark a widget and everything below it right-to-left (no-op in LTR locales).
///
/// GTK's widget direction is NOT inherited: `set_direction` on the window
/// leaves every child at LTR (the video-player settled this by screen
/// capture). That is why this walks the subtree instead of setting the window.
fn apply_rtl(root: &impl gtk::prelude::IsA<gtk::Widget>) {
    if !i18n::is_rtl() {
        return;
    }
    let root = root.as_ref();
    root.set_direction(gtk::TextDirection::Rtl);
    let mut child = root.first_child();
    while let Some(widget) = child {
        apply_rtl(&widget);
        child = widget.next_sibling();
    }
}

pub fn build(app: &adw::Application, ui: &Ui) {
    let stack = adw::ViewStack::new();
    let overlay = adw::ToastOverlay::new();
    *ui.overlay.borrow_mut() = Some(overlay.clone());

    let window = adw::ApplicationWindow::new(app);
    window.set_title(Some(&i18n::t("app.name")));
    window.set_default_size(900, 650);

    // AdwApplicationWindow owns its chrome: the header goes into an
    // AdwToolbarView, which becomes the window's content.
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let switcher = adw::ViewSwitcher::new();
    switcher.set_stack(Some(&stack));
    header.set_title_widget(Some(&switcher));

    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh.add_css_class("flat");
    refresh.set_tooltip_text(Some(&i18n::t("header.refresh")));
    {
        let ui = ui.clone();
        refresh.connect_clicked(move |_| ui.start_txn(Txn::Refresh));
    }
    header.pack_end(&refresh);
    *ui.refresh.borrow_mut() = Some(refresh);
    toolbar.add_top_bar(&header);

    let (installed_page, installed_root) = installed::build(ui);
    let (browse_page, browse_root) = browse::build(ui);
    let (distro_page, distro_root) = distro::build();
    let (aur_page, aur_root) = aur::build(ui);
    let (updates_page, updates_root) = updates::build(ui);
    let page = stack.add_titled(&installed_root, Some("installed"), &i18n::t("tabs.installed"));
    page.set_icon_name(Some("package-x-generic-symbolic"));
    let page = stack.add_titled(&browse_root, Some("browse"), &i18n::t("tabs.browse"));
    page.set_icon_name(Some("system-search-symbolic"));
    // Aelum: the distro's own apps not yet on the system, next to Browse
    // (it is Browse narrowed to the `[aelum]` repo) and before Updates.
    let page = stack.add_titled(&distro_root, Some("aelum"), &i18n::t("tabs.aelum"));
    page.set_icon_name(Some("starred-symbolic"));
    *ui.aelum_tab.borrow_mut() = Some(page);
    // AUR: the Arch User Repository. It sits with the other ways to get a
    // package and, unlike them, says nothing until asked — there is no badge
    // here because a badge would mean a query the user never made.
    let page = stack.add_titled(&aur_root, Some("aur"), &i18n::t("tabs.aur"));
    page.set_icon_name(Some("network-server-symbolic"));
    // Updates last: it is the page you go to when something is wrong, not
    // the one you open to look at your system. Its badge is the signal.
    let page = stack.add_titled(&updates_root, Some("updates"), &i18n::t("tabs.updates"));
    page.set_icon_name(Some("software-update-available-symbolic"));
    *ui.updates_tab.borrow_mut() = Some(page);

    // Visiting a badged page is the acknowledgement: its dot clears, and
    // while the page is up no dot is raised for what is on screen.
    {
        let ui = ui.clone();
        stack.connect_visible_child_name_notify(move |stack| {
            let name = stack.visible_child_name();
            let here = name.as_ref().map(|n| n.as_str()).unwrap_or("");
            let on_updates = here == "updates";
            ui.on_updates_page.set(on_updates);
            if on_updates
                && let Some(tab) = ui.updates_tab.borrow().as_ref()
            {
                tab.set_needs_attention(false);
            }
            let on_aelum = here == "aelum";
            ui.on_aelum_page.set(on_aelum);
            if on_aelum
                && let Some(tab) = ui.aelum_tab.borrow().as_ref()
            {
                tab.set_needs_attention(false);
            }
        });
    }

    // A running refresh replaces the page area (Ui::set_refresh_running
    // switches the two children): until the new databases are read, every row
    // on screen is the state from *before* the check, and stale rows read like
    // a result. The progress page is the honest content while it runs.
    let content = gtk::Stack::new();
    content.set_hexpand(true);
    content.set_vexpand(true);
    content.add_named(&stack, Some("pages"));
    content.add_named(&refreshing_page(), Some("refreshing"));
    content.set_visible_child_name("pages");
    *ui.content.borrow_mut() = Some(content.clone());

    toolbar.set_content(Some(&content));
    overlay.set_child(Some(&toolbar));
    window.set_content(Some(&overlay));

    *ui.window.borrow_mut() = Some(window.clone());
    *ui.installed.borrow_mut() = Some(installed_page);
    *ui.updates.borrow_mut() = Some(updates_page);
    *ui.browse.borrow_mut() = Some(browse_page);
    *ui.distro.borrow_mut() = Some(distro_page);
    *ui.aur_page.borrow_mut() = Some(aur_page);
    // RTL after the whole tree exists: every control must be covered.
    apply_rtl(&window);
    window.present();
}

/// The page that stands in for the tabs while a database refresh runs: a
/// spinner and the one sentence that explains why the content is gone.
/// Styled as the other full-area states (adw::StatusPage) so a page that
/// deliberately shows nothing and a page that is waiting look the same.
fn refreshing_page() -> adw::StatusPage {
    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    spinner.set_size_request(32, 32);
    let page = adw::StatusPage::builder()
        .title(i18n::t("status.refreshing"))
        .build();
    page.set_child(Some(&spinner));
    page
}
