#[cfg(not(test))]
use std::cell::RefCell;
use std::thread;

use iced::{Subscription, futures::channel::mpsc as iced_mpsc, stream};
#[cfg(not(test))]
use muda::{
    Menu, MenuItem, PredefinedMenuItem, Submenu,
    accelerator::{Accelerator, Code, Modifiers},
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

fn command_for_id(id: &MenuId) -> Command {
    match id.as_ref() {
        ID_SETTINGS => Command::Settings,
        ID_OPEN_GMA => Command::OpenGma,
        ID_ROUTE_MY_WORKSHOP => Command::Navigate(Route::MyWorkshop),
        ID_ROUTE_INSTALLED_ADDONS => Command::Navigate(Route::InstalledAddons),
        ID_ROUTE_DOWNLOADER => Command::Navigate(Route::Downloader),
        ID_ROUTE_SIZE_ANALYZER => Command::Navigate(Route::SizeAnalyzer),
        ID_GITHUB => Command::OpenUrl(GITHUB_URL),
        ID_REPORT_ISSUE => Command::OpenUrl(REPORT_ISSUE_URL),
        ID_UPSTREAM => Command::OpenUrl(crate::features::shell::UPSTREAM_REPO_URL),
        _ => Command::Unknown(id.clone()),
    }
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
    let settings = MenuItem::with_id(
        ID_SETTINGS,
        i18n.tr("menu-settings"),
        true,
        Some(accel(Modifiers::META, Code::Comma)),
    );
    let services = PredefinedMenuItem::services(None);
    let hide = PredefinedMenuItem::hide(None);
    let hide_others = PredefinedMenuItem::hide_others(None);
    let show_all = PredefinedMenuItem::show_all(None);
    let quit = PredefinedMenuItem::quit(None);

    Submenu::with_items(
        i18n.tr("gmpublished-name"),
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
    let open_gma = MenuItem::with_id(
        ID_OPEN_GMA,
        i18n.tr("menu-open-gma"),
        true,
        Some(accel(Modifiers::META, Code::KeyO)),
    );
    Submenu::with_items(i18n.tr("menu-file"), true, &[&open_gma])
}

#[cfg(not(test))]
fn go_menu(i18n: &I18n) -> muda::Result<Submenu> {
    let my_workshop = route_item(i18n, Route::MyWorkshop, ID_ROUTE_MY_WORKSHOP, Code::Digit1);
    let installed_addons = route_item(
        i18n,
        Route::InstalledAddons,
        ID_ROUTE_INSTALLED_ADDONS,
        Code::Digit2,
    );
    let downloader = route_item(i18n, Route::Downloader, ID_ROUTE_DOWNLOADER, Code::Digit3);
    let size_analyzer = route_item(
        i18n,
        Route::SizeAnalyzer,
        ID_ROUTE_SIZE_ANALYZER,
        Code::Digit4,
    );
    Submenu::with_items(
        i18n.tr("menu-go"),
        true,
        &[&my_workshop, &installed_addons, &downloader, &size_analyzer],
    )
}

#[cfg(not(test))]
fn route_item(i18n: &I18n, route: Route, id: &'static str, code: Code) -> MenuItem {
    MenuItem::with_id(
        id,
        i18n.tr(route.label_key()),
        true,
        Some(accel(Modifiers::META, code)),
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
    let github = MenuItem::with_id(ID_GITHUB, i18n.tr("menu-github"), true, None);
    let report_issue = MenuItem::with_id(ID_REPORT_ISSUE, i18n.tr("menu-report-issue"), true, None);
    let upstream = MenuItem::with_id(ID_UPSTREAM, i18n.tr("menu-upstream"), true, None);
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

    #[test]
    fn ids_map_to_commands() {
        assert_eq!(command_for_id(&MenuId::new(ID_SETTINGS)), Command::Settings);
        assert_eq!(
            command_for_id(&MenuId::new(ID_ROUTE_SIZE_ANALYZER)),
            Command::Navigate(Route::SizeAnalyzer)
        );
        assert_eq!(
            command_for_id(&MenuId::new(ID_REPORT_ISSUE)),
            Command::OpenUrl(REPORT_ISSUE_URL)
        );
    }
}
