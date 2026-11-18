//! The window: header, view switcher, and the five pages.

use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use super::app::Ui;
use super::{aur, browse, distro, dnd, installed, updates};
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

/// The window's own CSS: two overrides the theme does not give us.
///
/// The frame around a scrolled list is the viewport's, not the content's:
/// the pages put `.card` on the ScrolledWindow and keep `.boxed-list` on
/// the list, because libadwaita's row styling is `list.boxed-list > row`
/// — a child selector, so the class cannot move. That class also paints a
/// card on the list itself, and that outline would run to the end of the
/// content instead of ending at the window; this strips just that painting.
///
/// The drop hint's dashed frame is the video-player's drop-to-play shape:
/// `.osd` draws the dim film and its white ink, so the border reads in
/// both themes without a `prefers-color-scheme` branch.
fn add_window_css(window: &adw::ApplicationWindow) {
    let css = gtk::CssProvider::new();
    // A stylesheet this file cannot parse is a bug in this file; GTK names
    // the failure on the terminal, and a missing override is only cosmetic.
    css.load_from_string(
        "scrolledwindow.card list.boxed-list { background: none; box-shadow: none; }
         label.drop-hint {
             margin: 14px;
             border: 3px dashed alpha(white, 0.85);
             border-radius: 10px;
             font-weight: 600;
             font-size: 1.4em;
         }",
    );
    gtk::style_context_add_provider_for_display(
        &gtk::prelude::RootExt::display(window),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

pub fn build(app: &adw::Application, ui: &Ui) {
    let stack = adw::ViewStack::new();
    let overlay = adw::ToastOverlay::new();
    *ui.overlay.borrow_mut() = Some(overlay.clone());

    let window = adw::ApplicationWindow::new(app);
    add_window_css(&window);
    window.set_title(Some(&i18n::t("app.name")));
    window.set_default_size(900, 650);

    // AdwApplicationWindow owns its chrome: the header goes into an
    // AdwToolbarView, which becomes the window's content.
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    // The window controls move into the title's end box (below). With the
    // header's own end bin empty, the title widget gets the full width:
    // a packed end bin reserves its width on BOTH sides of the title
    // (gtkcenterlayout: center_size = size - 2 * MAX(start, end)), which
    // would off-center everything the title centers.
    header.set_show_end_title_buttons(false);

    let switcher = adw::ViewSwitcher::new();
    switcher.set_stack(Some(&stack));

    // The mirror date, left of the switcher: it is a fact about the
    // databases every page reads, not about one page. Two centered lines,
    // centered in the space between the window edge and the switcher —
    // the CenterBox's start child gets exactly that space (hexpand +
    // centered xalign), and its center child keeps the switcher
    // window-centered. (A pack_start label can't do this: AdwHeaderBar
    // pins its start box to the edge, halign START.) Hidden until a check
    // or a database mtime proves one happened; Ui::refresh_pages fills it.
    let fresh = gtk::Label::new(None);
    fresh.add_css_class("dim-label");
    fresh.add_css_class("caption");
    fresh.set_justify(gtk::Justification::Center);
    fresh.set_xalign(0.5);
    fresh.set_hexpand(true);
    fresh.set_valign(gtk::Align::Center);
    fresh.set_visible(false);
    *ui.fresh.borrow_mut() = Some(fresh.clone());

    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh.add_css_class("flat");
    refresh.set_tooltip_text(Some(&i18n::t("header.refresh")));
    {
        let ui = ui.clone();
        refresh.connect_clicked(move |_| ui.start_txn(Txn::Refresh));
    }

    // The buttons live in the title's end box, not the header's: the title
    // owns the full width now (see show-end-title-buttons above). Append
    // order is left-to-right — refresh, then ⋯, then close outermost-
    // right; the box hugs the right edge.
    //
    // Close is a real button, not `gtk::WindowControls`: this window draws
    // its own chrome and asks the compositor to decorate the frame, and
    // Hyprland (which implements xdg-decoration as server-side and draws
    // no buttons) leaves WindowControls empty — measured, and it made the
    // manager a window with no way out. `window.close` is GtkWindow's own
    // action: it emits the close request and destroys the window exactly
    // like the compositor's kill would.
    let close = gtk::Button::from_icon_name("window-close-symbolic");
    close.add_css_class("flat");
    close.set_tooltip_text(Some(&i18n::t("action.close")));
    close.set_action_name(Some("window.close"));
    // The "⋯" menu: the app-level acts — updating the mirrors, About, and
    // leaving. They are `app.` actions because the application, not this
    // window, owns them: the window is one-shot, the process is not. A
    // MenuButton over a hand-rolled popover, so the panel is the one GNOME
    // draws identically everywhere, and the labels come from the catalogs
    // like everything else.
    let menu = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text(i18n::t("mirrors.menu"))
        .build();
    menu.add_css_class("flat");
    let items = gtk::gio::Menu::new();
    items.append(Some(&i18n::t("mirrors.title")), Some("app.update-mirrors"));
    items.append(Some(&i18n::t("about.label")), Some("app.about"));
    items.append(Some(&i18n::t("action.exit")), Some("app.quit"));
    menu.set_menu_model(Some(&items));
    let end = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    end.set_halign(gtk::Align::End);
    end.append(&refresh);
    end.append(&menu);
    end.append(&close);
    *ui.refresh.borrow_mut() = Some(refresh);

    let title = gtk::CenterBox::new();
    title.set_start_widget(Some(&fresh));
    title.set_center_widget(Some(&switcher));
    title.set_end_widget(Some(&end));
    title.set_hexpand(true);
    header.set_title_widget(Some(&title));

    toolbar.add_top_bar(&header);

    let (installed_page, installed_root) = installed::build(ui);
    let (browse_page, browse_root) = browse::build(ui);
    let (distro_page, distro_root) = distro::build();
    let (aur_page, aur_root) = aur::build(ui);
    let (updates_page, updates_root) = updates::build(ui);
    let page = stack.add_titled(
        &installed_root,
        Some("installed"),
        &i18n::t("tabs.installed"),
    );
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
            if on_updates && let Some(tab) = ui.updates_tab.borrow().as_ref() {
                tab.set_needs_attention(false);
            }
            let on_aelum = here == "aelum";
            ui.on_aelum_page.set(on_aelum);
            if on_aelum && let Some(tab) = ui.aelum_tab.borrow().as_ref() {
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

    // Drop feedback: a dashed frame and a hint drawn *over* the pages
    // (topmost overlay child), hidden until a file drag enters the window —
    // the video-player's shape, with the drop target on the window so a
    // file lands on the app wherever the pointer is.
    let drop_hint = gtk::Label::builder()
        .label(i18n::t("drop.hint"))
        .xalign(0.5)
        .yalign(0.5)
        .build();
    drop_hint.add_css_class("osd");
    drop_hint.add_css_class("drop-hint");
    drop_hint.set_halign(gtk::Align::Fill);
    drop_hint.set_valign(gtk::Align::Fill);
    drop_hint.set_visible(false);
    let area = gtk::Overlay::new();
    area.set_child(Some(&content));
    area.add_overlay(&drop_hint);

    toolbar.set_content(Some(&area));
    overlay.set_child(Some(&toolbar));
    window.set_content(Some(&overlay));

    *ui.window.borrow_mut() = Some(window.clone().upcast());
    *ui.installed.borrow_mut() = Some(installed_page);
    *ui.updates.borrow_mut() = Some(updates_page);
    *ui.browse.borrow_mut() = Some(browse_page);
    *ui.distro.borrow_mut() = Some(distro_page);
    *ui.aur_page.borrow_mut() = Some(aur_page);
    // The window is one-shot: closing it destroys it. Every slot `Ui`
    // keeps of this window's widgets belongs to it — clearing them here
    // is what makes a later double-click take the popup path again
    // instead of presenting (and parenting to) a destroyed window.
    {
        let ui = ui.clone();
        window.connect_destroy(move |_| ui.forget_window());
    }
    dnd::attach(&window, &drop_hint, ui);
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
