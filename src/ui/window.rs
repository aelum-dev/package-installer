//! The window: header, view switcher, and the four pages.

use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use super::app::Ui;
use super::{browse, distro, installed, updates};
use crate::txn::Txn;

pub fn build(app: &adw::Application, ui: &Ui) {
    let stack = adw::ViewStack::new();
    let overlay = adw::ToastOverlay::new();
    *ui.overlay.borrow_mut() = Some(overlay.clone());

    let window = adw::ApplicationWindow::new(app);
    window.set_title(Some("Packages"));
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
    refresh.set_tooltip_text(Some("Check for updates (refresh package databases)"));
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
    let (updates_page, updates_root) = updates::build(ui);
    let page = stack.add_titled(&installed_root, Some("installed"), "Installed");
    page.set_icon_name(Some("package-x-generic-symbolic"));
    let page = stack.add_titled(&browse_root, Some("browse"), "Browse");
    page.set_icon_name(Some("system-search-symbolic"));
    // Aelum: the distro's own apps not yet on the system, next to Browse
    // (it is Browse narrowed to the `[aelum]` repo) and before Updates.
    let page = stack.add_titled(&distro_root, Some("aelum"), "Aelum");
    page.set_icon_name(Some("starred-symbolic"));
    *ui.aelum_tab.borrow_mut() = Some(page);
    // Updates last: it is the page you go to when something is wrong, not
    // the one you open to look at your system. Its badge is the signal.
    let page = stack.add_titled(&updates_root, Some("updates"), "Updates");
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

    toolbar.set_content(Some(&stack));
    overlay.set_child(Some(&toolbar));
    window.set_content(Some(&overlay));

    *ui.window.borrow_mut() = Some(window.clone());
    *ui.installed.borrow_mut() = Some(installed_page);
    *ui.updates.borrow_mut() = Some(updates_page);
    *ui.browse.borrow_mut() = Some(browse_page);
    *ui.distro.borrow_mut() = Some(distro_page);
    window.present();
}
