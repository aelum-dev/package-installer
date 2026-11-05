//! The window: header, view switcher, and the two pages.

use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use super::app::Ui;
use super::{browse, installed};
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
    refresh.set_tooltip_text(Some("Refresh package databases"));
    {
        let ui = ui.clone();
        refresh.connect_clicked(move |_| ui.start_txn(Txn::Refresh));
    }
    header.pack_end(&refresh);
    toolbar.add_top_bar(&header);

    let (installed_page, installed_root) = installed::build(ui);
    let (browse_page, browse_root) = browse::build(ui);
    let page = stack.add_titled(&installed_root, Some("installed"), "Installed");
    page.set_icon_name(Some("package-x-generic-symbolic"));
    let page = stack.add_titled(&browse_root, Some("browse"), "Browse");
    page.set_icon_name(Some("system-search-symbolic"));

    toolbar.set_content(Some(&stack));
    overlay.set_child(Some(&toolbar));
    window.set_content(Some(&overlay));

    *ui.window.borrow_mut() = Some(window.clone());
    *ui.installed.borrow_mut() = Some(installed_page);
    *ui.browse.borrow_mut() = Some(browse_page);
    window.present();
}
