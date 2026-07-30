#[cfg(not(test))]
use std::cell::RefCell;
use std::thread;

use iced::{Subscription, futures::channel::mpsc as iced_mpsc, stream};
use muda::accelerator::Code;
#[cfg(not(test))]
use muda::{
    Menu, MenuItem, PredefinedMenuItem, Submenu,
    accelerator::{Accelerator, Modifiers},
};
use muda::{MenuEvent, MenuId};

use crate::features::shell::Route;
#[cfg(not(test))]
use crate::i18n::I18n;

#[cfg(not(test))]
thread_local! {
    // AppKit retains the native NSMenu, but muda menu items hold pointers into
    // this Rust menu tree. Keep it alive as long as the installed menu exists.
    static INSTALLED_MENU: RefCell<Option<Menu>> = const { RefCell::new(None) };
}

const ID_SETTINGS: &str = "menu.settings";
const ID_OPEN_GMA: &str = "menu.open-gma";
const ID_ROUTE_MY_WORKSHOP: &str = "menu.route.my-workshop";
const ID_ROUTE_INSTALLED_ADDONS: &str = "menu.route.installed-addons";
const ID_ROUTE_DOWNLOADER: &str = "menu.route.downloader";
const ID_ROUTE_SIZE_ANALYZER: &str = "menu.route.size-analyzer";
const ID_GITHUB: &str = "menu.github";
const ID_REPORT_ISSUE: &str = "menu.report-issue";
const ID_UPSTREAM: &str = "menu.upstream";

pub const GITHUB_URL: &str = "https://github.com/charles-mills/gmpublished";
pub const REPORT_ISSUE_URL: &str = "https://github.com/charles-mills/gmpublished/issues/new";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    Settings,
    OpenGma,
    Navigate(Route),
    OpenUrl(&'static str),
    Unknown(MenuId),
}

#[cfg(not(test))]
pub fn install(i18n: &I18n) {
    match build_menu(i18n).map(|menu| {
        menu.init_for_nsapp();
        INSTALLED_MENU.with(|installed| {
            installed.replace(Some(menu));
        });
    }) {
        Ok(()) => {}
        Err(error) => log::warn!("failed to install macOS menu bar: {error}"),
    }
}

pub fn subscription() -> Subscription<Command> {
    Subscription::run(menu_event_stream)
}

fn menu_event_stream() -> impl iced::futures::Stream<Item = Command> + use<> {
    stream::channel(16, async move |output| {
        let spawned = thread::Builder::new()
            .name("macos-menu-event-drain".to_owned())
            .spawn(move || forward_menu_events(output));
        if let Err(error) = spawned {
            log::warn!("failed to spawn macOS menu event forwarder: {error}");
        }
    })
}

fn forward_menu_events(mut output: iced_mpsc::Sender<Command>) {
    let receiver = MenuEvent::receiver();
    while let Ok(event) = receiver.recv() {
        if !crate::util::channel::send_blocking(&mut output, command_for_id(event.id())) {
            return;
        }
    }
}

/// Every menu item that dispatches a command.
///
/// Both the builders and [`command_for_id`] read this, so an id cannot be
/// spelled in one and missed in the other.
///
/// Predefined items (about, quit, minimise, ...) are not here: AppKit
/// dispatches those itself and they never reach [`command_for_id`].
struct CommandItem {
    id: &'static str,
    label_key: &'static str,
    command: Command,
    /// Command-key shortcut, if the item has one.
    shortcut: Option<Code>,
}

static SETTINGS: CommandItem = CommandItem {
    id: ID_SETTINGS,
    label_key: "menu-settings",
    command: Command::Settings,
    shortcut: Some(Code::Comma),
};

static OPEN_GMA: CommandItem = CommandItem {
    id: ID_OPEN_GMA,
    label_key: "menu-open-gma",
    command: Command::OpenGma,
    shortcut: Some(Code::KeyO),
};

static MY_WORKSHOP: CommandItem = CommandItem {
    id: ID_ROUTE_MY_WORKSHOP,
    label_key: Route::MyWorkshop.label_key(),
    command: Command::Navigate(Route::MyWorkshop),
    shortcut: Some(Code::Digit1),
};

static INSTALLED_ADDONS: CommandItem = CommandItem {
    id: ID_ROUTE_INSTALLED_ADDONS,
    label_key: Route::InstalledAddons.label_key(),
    command: Command::Navigate(Route::InstalledAddons),
    shortcut: Some(Code::Digit2),
};

static DOWNLOADER: CommandItem = CommandItem {
    id: ID_ROUTE_DOWNLOADER,
    label_key: Route::Downloader.label_key(),
    command: Command::Navigate(Route::Downloader),
    shortcut: Some(Code::Digit3),
};

static SIZE_ANALYZER: CommandItem = CommandItem {
    id: ID_ROUTE_SIZE_ANALYZER,
    label_key: Route::SizeAnalyzer.label_key(),
    command: Command::Navigate(Route::SizeAnalyzer),
    shortcut: Some(Code::Digit4),
};

static GITHUB: CommandItem = CommandItem {
    id: ID_GITHUB,
    label_key: "menu-github",
    command: Command::OpenUrl(GITHUB_URL),
    shortcut: None,
};

static REPORT_ISSUE: CommandItem = CommandItem {
    id: ID_REPORT_ISSUE,
    label_key: "menu-report-issue",
    command: Command::OpenUrl(REPORT_ISSUE_URL),
    shortcut: None,
};

static UPSTREAM: CommandItem = CommandItem {
    id: ID_UPSTREAM,
    label_key: "menu-upstream",
    command: Command::OpenUrl(crate::features::shell::UPSTREAM_REPO_URL),
    shortcut: None,
};

/// Every item in [`COMMAND_ITEMS`], for the dispatcher to scan.
static COMMAND_ITEMS: &[&CommandItem] = &[
    &SETTINGS,
    &OPEN_GMA,
    &MY_WORKSHOP,
    &INSTALLED_ADDONS,
    &DOWNLOADER,
    &SIZE_ANALYZER,
    &GITHUB,
    &REPORT_ISSUE,
    &UPSTREAM,
];

fn command_item(id: &str) -> Option<&'static CommandItem> {
    COMMAND_ITEMS.iter().copied().find(|item| item.id == id)
}

fn command_for_id(id: &MenuId) -> Command {
    command_item(id.as_ref())
        .map_or_else(|| Command::Unknown(id.clone()), |item| item.command.clone())
}

#[cfg(not(test))]
fn build_menu(i18n: &I18n) -> muda::Result<Menu> {
    let app = app_menu(i18n)?;
    let file = file_menu(i18n)?;
    let go = go_menu(i18n)?;
    let window = window_menu(i18n)?;
    let help = help_menu(i18n)?;
    Menu::with_items(&[&app, &file, &go, &window, &help])
}

#[cfg(not(test))]
fn app_menu(i18n: &I18n) -> muda::Result<Submenu> {
    let about = PredefinedMenuItem::about(None, None);
    let settings = command_menu_item(i18n, &SETTINGS);
    let services = PredefinedMenuItem::services(None);
    let hide = PredefinedMenuItem::hide(None);
    let hide_others = PredefinedMenuItem::hide_others(None);
    let show_all = PredefinedMenuItem::show_all(None);
    let quit = PredefinedMenuItem::quit(None);

    Submenu::with_items(
        crate::APP_NAME,
        true,
        &[
            &about,
            &PredefinedMenuItem::separator(),
            &settings,
            &PredefinedMenuItem::separator(),
            &services,
            &PredefinedMenuItem::separator(),
            &hide,
            &hide_others,
            &show_all,
            &PredefinedMenuItem::separator(),
            &quit,
        ],
    )
}

#[cfg(not(test))]
fn file_menu(i18n: &I18n) -> muda::Result<Submenu> {
    let open_gma = command_menu_item(i18n, &OPEN_GMA);
    Submenu::with_items(i18n.tr("menu-file"), true, &[&open_gma])
}

#[cfg(not(test))]
fn go_menu(i18n: &I18n) -> muda::Result<Submenu> {
    let my_workshop = command_menu_item(i18n, &MY_WORKSHOP);
    let installed_addons = command_menu_item(i18n, &INSTALLED_ADDONS);
    let downloader = command_menu_item(i18n, &DOWNLOADER);
    let size_analyzer = command_menu_item(i18n, &SIZE_ANALYZER);
    Submenu::with_items(
        i18n.tr("menu-go"),
        true,
        &[&my_workshop, &installed_addons, &downloader, &size_analyzer],
    )
}

/// Builds a menu item from its declaration, so its label and shortcut come
/// from the same place its command does. Takes the item rather than an id: a
/// lookup could miss, and a builder's only recourse would be to panic.
#[cfg(not(test))]
fn command_menu_item(i18n: &I18n, item: &'static CommandItem) -> MenuItem {
    MenuItem::with_id(
        item.id,
        i18n.tr(item.label_key),
        true,
        item.shortcut.map(|code| accel(Modifiers::META, code)),
    )
}

#[cfg(not(test))]
fn window_menu(i18n: &I18n) -> muda::Result<Submenu> {
    let minimize = PredefinedMenuItem::minimize(Some(&i18n.tr("menu-minimize")));
    let zoom = PredefinedMenuItem::maximize(Some(&i18n.tr("menu-zoom")));
    Submenu::with_items(i18n.tr("menu-window"), true, &[&minimize, &zoom])
}

#[cfg(not(test))]
fn help_menu(i18n: &I18n) -> muda::Result<Submenu> {
    let github = command_menu_item(i18n, &GITHUB);
    let report_issue = command_menu_item(i18n, &REPORT_ISSUE);
    let upstream = command_menu_item(i18n, &UPSTREAM);
    Submenu::with_items(
        i18n.tr("menu-help"),
        true,
        &[
            &github,
            &report_issue,
            &PredefinedMenuItem::separator(),
            &upstream,
        ],
    )
}

#[cfg(not(test))]
fn accel(modifiers: Modifiers, code: Code) -> Accelerator {
    Accelerator::new(Some(modifiers), code)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `command_for_id` resolves by scanning the table, so this cannot check
    /// that a row maps to its own command — that holds by construction. What
    /// it can check is that the table is *usable* as a lookup: an id appearing
    /// twice makes the second row unreachable, silently, because the scan
    /// stops at the first match.
    #[test]
    fn no_menu_id_is_claimed_twice() {
        let mut seen = Vec::new();
        for item in COMMAND_ITEMS {
            assert!(
                !seen.contains(&item.id),
                "{} is declared by two menu items; the second is unreachable",
                item.id
            );
            seen.push(item.id);
        }
    }

    /// An id AppKit sends that no item claims must round-trip as `Unknown`
    /// rather than being mistaken for one of ours.
    #[test]
    fn an_unclaimed_id_dispatches_unknown() {
        assert!(matches!(
            command_for_id(&MenuId::new("menu.not-a-real-item")),
            Command::Unknown(_)
        ));
        assert_eq!(
            command_for_id(&MenuId::new(ID_SETTINGS)),
            Command::Settings,
            "a claimed id must not fall through to Unknown"
        );
    }

    /// Menu labels go through Fluent like every other string. A missing key
    /// renders the raw key in the macOS menu bar, where nothing else would
    /// catch it.
    #[test]
    fn every_menu_label_key_has_a_catalog_entry() {
        let i18n = crate::i18n::I18n::for_locale(Some("en"));
        for item in COMMAND_ITEMS {
            assert_ne!(
                i18n.tr(item.label_key),
                item.label_key,
                "{} has no catalog entry",
                item.label_key
            );
        }
    }

    /// Two items sharing a Command-key shortcut means one of them is
    /// unreachable from the keyboard, and which one is up to AppKit.
    #[test]
    fn no_two_menu_items_claim_the_same_shortcut() {
        let mut seen = Vec::new();
        for shortcut in COMMAND_ITEMS.iter().filter_map(|item| item.shortcut) {
            assert!(
                !seen.contains(&shortcut),
                "{shortcut:?} is claimed by two menu items"
            );
            seen.push(shortcut);
        }
    }
}
