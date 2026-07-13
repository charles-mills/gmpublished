#[cfg(feature = "debug")]
use std::collections::HashSet;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use iced::futures::channel::mpsc as iced_mpsc;
use iced::widget::{Space, column, container, mouse_area, row, sensor, stack, text};
use iced::{
    Element, Event, Length, Padding, Point, Size, Subscription, Task, Theme, event, keyboard,
    mouse, stream, system, theme::Mode, window,
};

use crate::backend::{
    DownloadCountFormat, Settings, SystemColorScheme, ThemePreset,
    domain::{
        PublishedFileId, SearchFullRequest, SearchMode, SearchQuickRequest, WORKSHOP_LEGAL_URL,
        WorkshopDownloadResult, WorkshopDownloadSuccess, workshop_url,
    },
    effective_theme_preset, gma,
    library::{LibraryRefresh, LibraryRefreshReason, LibrarySnapshot},
    library_watch,
    native::NativeOpenTarget,
    tasks::{
        self, BackendContext, BackendRuntimeAction, BackendRuntimeEvent, BackendServices,
        RunBlockingError, TaskEvent, TaskHandle, TaskKind,
    },
    ui_error::UiError,
};
use crate::features::{
    context_menu, destination_select, downloader, file_preview, installed_addons, modal_stack,
    my_workshop, prepare_publish, preview_gma, search, settings, shell, size_analyzer,
    steam_session, tasks_overlay,
};
use crate::format::DownloadCountFormatter;
use crate::i18n::I18n;
use crate::media::{sounds, thumbnail_demand};
#[cfg(target_os = "macos")]
use crate::platform_menu;
use crate::theme::{self, Tokens};
use crate::widgets::{addon_grid, shortcut_capture::shortcut_capture};

const ADDON_DRAG_THRESHOLD: f32 = 6.0;
const STEAM_WORKSHOP_URL: &str = "https://steamcommunity.com/app/4000/workshop/";
const MAX_DROPPED_TEXT_BYTES: u64 = 1024 * 1024;
const WORKSHOP_DRAG_PREFIX: &str = "gmpublished/workshop-id:";

mod drag;
mod routes;
mod runners;
mod side_effects_addons;
#[cfg(feature = "asset-studio")]
mod side_effects_audio;
mod side_effects_downloader;
#[cfg(feature = "asset-studio")]
mod side_effects_file_preview;
mod side_effects_preview_gma;
mod side_effects_publish;
mod side_effects_search;
mod side_effects_settings;
mod side_effects_shell;
mod side_effects_size_analyzer;
mod side_effects_steam;
mod side_effects_thumbnails;
mod view_support;

#[cfg(test)]
mod tests;

#[cfg(test)]
use drag::AddonDragOutcome;
use drag::{
    AddonDragMessage, AddonDragSource, AddonDragState, addon_drag_event, file_drop_event,
    parse_dropped_workshop_ids,
};
use routes::{RouteLifecycle, open_modal_message};
#[cfg(target_os = "macos")]
use runners::run_document_open_extraction;
use runners::{
    backend_runtime_action_message, flatten_blocking_ui_result, run_downloader_local_extraction,
    run_downloader_submission, run_installed_metadata_refresh, run_preview_gma_archive_extraction,
    run_preview_gma_entry_extraction, run_search_full, run_size_analyzer_preview_urls,
    schedule_native_open_target, send_root_message, spawn_blocking_detached_or_warn,
};
#[cfg(feature = "asset-studio")]
use side_effects_audio::AudioPlayback;
use side_effects_shell::ContextMenuTarget;
#[cfg(test)]
use side_effects_shell::LocalMenuTarget;
use side_effects_thumbnails::log_thumbnail_delivery;
use view_support::{addon_drag_ghost, resolve_tokens, system_scheme_from_mode};

#[derive(Debug)]
pub struct App {
    ctx: BackendContext,
    thumbnails: thumbnail_demand::Manager,
    state: State,
    window_id: Option<window::Id>,
    /// One warm pass per session; set when the first library snapshot kicks it.
    library_warm_kicked: bool,
    #[cfg(feature = "asset-studio")]
    audio_playback: Option<AudioPlayback>,
}

impl Drop for App {
    /// Iced drops the root model when the event loop ends, so this is where
    /// app quit reaches Steam's background threads: signal and join them
    /// here rather than leaving process exit to race a still-running
    /// connect retry or callback pump.
    fn drop(&mut self) {
        self.ctx.backend().steam.shutdown();
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct State {
    title: &'static str,
    shell: shell::State,
    my_workshop: my_workshop::State,
    installed_addons: installed_addons::State,
    downloader: downloader::State,
    size_analyzer: size_analyzer::State,
    search: search::State,
    destination_select: destination_select::State,
    file_preview: file_preview::State,
    prepare_publish: prepare_publish::State,
    preview_gma: preview_gma::State,
    settings: settings::State,
    context_menu: context_menu::State,
    steam_session: steam_session::State,
    modal_stack: modal_stack::State,
    tasks_overlay: tasks_overlay::State,
    chrome_strategy: shell::ChromeStrategy,
    theme_preset: ThemePreset,
    download_count_format: DownloadCountFormat,
    system_scheme: SystemColorScheme,
    accent_inputs: theme::AccentInputs,
    tokens: Tokens,
    addon_drag: AddonDragState,
    context_menu_target: Option<ContextMenuTarget>,
    context_menu_extract_paths: Option<Vec<PathBuf>>,
    #[cfg(feature = "debug")]
    hidden_workshop_ids: HashSet<PublishedFileId>,
    #[cfg(feature = "debug")]
    hidden_addon_paths: HashSet<PathBuf>,
    viewport_size: Size,
    i18n: I18n,
}

impl Default for State {
    fn default() -> Self {
        let theme_preset = ThemePreset::default();
        let download_count_format = Settings::default().download_count_format;
        let system_scheme = SystemColorScheme::Dark;
        let accent_inputs = theme::AccentInputs::for_preset(theme_preset);
        let mut state = Self {
            title: "app-title",
            shell: shell::State::default(),
            my_workshop: my_workshop::State::default(),
            installed_addons: installed_addons::State::default(),
            downloader: downloader::State::default(),
            size_analyzer: size_analyzer::State::default(),
            search: search::State::default(),
            destination_select: destination_select::State::default(),
            file_preview: file_preview::State::default(),
            prepare_publish: prepare_publish::State::default(),
            preview_gma: preview_gma::State::default(),
            settings: settings::State::default(),
            context_menu: context_menu::State::default(),
            steam_session: steam_session::State::default(),
            modal_stack: modal_stack::State::default(),
            tasks_overlay: tasks_overlay::State::default(),
            chrome_strategy: shell::ChromeStrategy::resolve(Settings::default().titlebar),
            theme_preset,
            download_count_format,
            system_scheme,
            accent_inputs,
            tokens: resolve_tokens(theme_preset, system_scheme, accent_inputs),
            addon_drag: AddonDragState::default(),
            context_menu_target: None,
            context_menu_extract_paths: None,
            #[cfg(feature = "debug")]
            hidden_workshop_ids: HashSet::new(),
            #[cfg(feature = "debug")]
            hidden_addon_paths: HashSet::new(),
            viewport_size: Size::ZERO,
            i18n: I18n::from_user_or_system(None),
        };
        state.apply_localized_labels();
        state
    }
}

impl State {
    fn follows_system_theme(&self) -> bool {
        self.theme_preset == ThemePreset::Auto
    }

    fn set_play_gifs_by_default(&mut self, enabled: bool) {
        let _ = self.my_workshop.set_play_gifs_by_default(enabled);
        let _ = self.installed_addons.set_play_gifs_by_default(enabled);
    }

    /// GIFs never play while the window is unfocused: every playback site
    /// pauses on its current frame and its clock subscription drops, so a
    /// backgrounded window idles at 0% CPU.
    fn set_window_focused(&mut self, focused: bool) {
        let _ = self.my_workshop.set_window_focused(focused);
        let _ = self.installed_addons.set_window_focused(focused);
        let _ = self.prepare_publish.set_window_focused(focused);
        let _ = self.preview_gma.set_window_focused(focused);
    }

    fn apply_runtime_settings(&mut self, settings: &Settings) {
        self.theme_preset = settings.theme_preset;
        self.accent_inputs = settings::accent_inputs_from_settings(settings);
        self.tokens = resolve_tokens(self.theme_preset, self.system_scheme, self.accent_inputs);
        self.chrome_strategy = shell::ChromeStrategy::resolve(settings.titlebar);
        self.download_count_format = settings.download_count_format;
        self.apply_runtime_language(settings.language.as_deref());
        self.set_play_gifs_by_default(settings.play_gifs_by_default);
        self.apply_download_count_formatter();
    }

    fn apply_runtime_language(&mut self, language: Option<&str>) {
        if let Some(language) = language {
            self.i18n.select_locale(Some(language));
        } else {
            self.i18n = I18n::from_user_or_system(None);
        }
        self.apply_localized_labels();
        self.apply_download_count_formatter();
    }

    fn apply_localized_labels(&mut self) {
        let _ = self
            .my_workshop
            .set_publish_new_title(self.i18n.tr("publish-new"));
    }

    fn download_count_formatter(&self) -> DownloadCountFormatter {
        DownloadCountFormatter::from_format_and_locale(
            self.download_count_format,
            Some(self.i18n.locale_id()),
        )
    }

    fn apply_download_count_formatter(&mut self) {
        let formatter = self.download_count_formatter();
        let _ = self.my_workshop.set_download_count_formatter(formatter);
        let _ = self
            .installed_addons
            .set_download_count_formatter(formatter);
        let _ = self.preview_gma.set_download_count_formatter(formatter);
    }

    fn apply_system_theme(&mut self, mode: Mode) {
        self.system_scheme = system_scheme_from_mode(mode);
        self.settings.apply_system_scheme(self.system_scheme);

        if self.follows_system_theme() {
            self.tokens = resolve_tokens(self.theme_preset, self.system_scheme, self.accent_inputs);
        }
    }

    fn needs_motion_ticks(&self) -> bool {
        self.modal_stack.needs_ticks()
            || self.context_menu.needs_ticks()
            || self.shell.needs_motion_ticks()
            || self.search.needs_motion_ticks()
            || self.tasks_overlay.needs_ticks()
            || self.prepare_publish.browser_select_hover_needs_ticks()
            || self.downloader.needs_progress_ticks()
    }
}

#[derive(Clone, Debug)]
pub enum RootMessage {
    Shell(shell::Message),
    MyWorkshop(my_workshop::Message),
    InstalledAddons(installed_addons::Message),
    Downloader(downloader::Message),
    SizeAnalyzer(size_analyzer::Message),
    Search(search::Message),
    DestinationSelect(destination_select::Message),
    FilePreview(file_preview::Message),
    PreparePublish(prepare_publish::Message),
    PreviewGma(preview_gma::Message),
    Settings(settings::Message),
    ContextMenu(context_menu::Message),
    SteamSession(steam_session::Message),
    ModalStack(modal_stack::Message),
    TasksOverlay(tasks_overlay::Message),
    SystemThemeObserved(Mode),
    DragRegionPressed,
    DragRegionDoubleClicked,
    TaskEvent(TaskEvent),
    BackendEvent(BackendRuntimeEvent),
    LibraryWatch(library_watch::Message),
    LibraryRefreshRequested(LibraryRefreshReason),
    LibraryRefreshed(
        LibraryRefreshReason,
        Result<LibraryRefresh, RunBlockingError>,
    ),
    UpdateCheckCompleted(
        Result<
            Result<Option<shell::UpdateRelease>, Arc<shell::UpdateCheckError>>,
            RunBlockingError,
        >,
    ),
    ThumbnailDemand(thumbnail_demand::Message),
    /// Cached preview URLs for the whole library, resolved once per session
    /// to warm the thumbnail disk cache in the background.
    WarmLibraryResolved(Vec<(PublishedFileId, String)>),
    WindowEvent(window::Id, window::Event),
    WindowScaleFactorObserved(window::Id, f32),
    AddonDrag(AddonDragMessage),
    LayoutObserved(Size),
    GlobalShortcut(GlobalShortcut),
    #[cfg(target_os = "macos")]
    Menu(platform_menu::Command),
    #[cfg(target_os = "macos")]
    MenuOpenGmaCompleted(Option<PathBuf>),
    /// The system flipped between light and dark appearance; AppKit resets
    /// the custom traffic-light frames during that re-layout.
    #[cfg(target_os = "macos")]
    SystemAppearanceChanged,
    FileDropped(PathBuf),
    /// `.gma` documents were opened via the OS file association (macOS
    /// double-click / "Open With"), delivered by the platform-open bridge.
    #[cfg(target_os = "macos")]
    GmaDocumentsOpened(Vec<PathBuf>),
    AnimationTick(Instant),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GlobalShortcut {
    ToggleSearch,
    #[cfg(feature = "asset-studio")]
    ToggleFileSearch,
    ToggleSettings,
    NavigateRoute(shell::Route),
}

fn sync_search_installed_addons(
    search: &gmpublished_backend::search::Search,
    snapshot: Option<&LibrarySnapshot>,
) {
    let items = snapshot.map_or_else(Vec::new, search_items_from_library);
    search.sync_installed_addons(items);
    #[cfg(feature = "asset-studio")]
    {
        let file_items = snapshot.map_or_else(Vec::new, search_file_items_from_library);
        search.sync_installed_addon_files(file_items);
    }
}

fn search_items_from_library(
    snapshot: &LibrarySnapshot,
) -> Vec<gmpublished_backend::search::SearchItem> {
    snapshot
        .addons
        .iter()
        .map(|addon| {
            let metadata = &addon.meta.header.metadata;
            let mut terms = metadata.tags().cloned().unwrap_or_default();
            if let Some(addon_type) = metadata.addon_type() {
                terms.push(addon_type.to_owned());
            }
            if let Some(workshop_id) = addon.workshop_id {
                terms.push(workshop_id.get().to_string());
            }

            gmpublished_backend::search::SearchItem::new_installed_addon(
                addon.canonical_path.clone(),
                addon.workshop_id.map(PublishedFileId::get),
                addon.display_title(),
                terms,
                addon.modified_epoch_seconds,
            )
        })
        .collect()
}

#[cfg(feature = "asset-studio")]
fn search_file_items_from_library(
    snapshot: &LibrarySnapshot,
) -> Vec<gmpublished_backend::search::SearchItem> {
    snapshot
        .addons
        .iter()
        .flat_map(|addon| {
            let addon_title = addon.display_title();
            let workshop_id = addon.workshop_id.map(PublishedFileId::get);
            let canonical_path = addon.canonical_path.clone();
            addon.meta.entries.iter().map(move |entry| {
                let label = archive_entry_file_name(&entry.path).to_owned();
                let mut terms = vec![entry.path.clone(), addon_title.clone()];
                if let Some(workshop_id) = workshop_id {
                    terms.push(workshop_id.to_string());
                }
                if let Some(extension) = archive_entry_extension(&entry.path) {
                    terms.push(extension.to_owned());
                }

                gmpublished_backend::search::SearchItem::new_installed_addon_file(
                    gmpublished_backend::search::InstalledAddonFileInfo {
                        addon_path: canonical_path.clone(),
                        addon_title: addon_title.clone(),
                        workshop_id,
                        entry_path: entry.path.clone(),
                        size_bytes: entry.size,
                        crc32: entry.crc32,
                    },
                    label,
                    terms,
                    addon.modified_epoch_seconds,
                )
            })
        })
        .collect()
}

#[cfg(feature = "asset-studio")]
fn archive_entry_file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

#[cfg(feature = "asset-studio")]
fn archive_entry_extension(path: &str) -> Option<&str> {
    archive_entry_file_name(path)
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .filter(|extension| !extension.is_empty())
}

impl App {
    #[cfg(test)]
    pub(crate) fn new_for_test() -> Self {
        Self::from_context(BackendContext::new().expect("test backend context"))
    }

    pub(crate) fn new(ctx: BackendContext) -> (Self, Task<RootMessage>) {
        let mut app = Self::from_context(ctx);
        let startup_tasks = app.startup_tasks();
        (app, startup_tasks)
    }

    /// Deterministic construction shared by production and tests. External
    /// startup work is deliberately kept in `startup_tasks`, so ordinary
    /// state/effect tests never launch HTTP, discovery, or warm-up jobs.
    fn from_context(ctx: BackendContext) -> Self {
        let thumbnails = thumbnail_demand::Manager::new(thumbnail_demand::Config {
            disk_cache_dir: gmpublished_backend::appdata::cache_dir()
                .map(|dir| dir.join("thumbnails")),
            ..thumbnail_demand::Config::default()
        });
        let mut state = State::default();
        state.set_play_gifs_by_default(ctx.play_gifs_by_default());
        let (settings, paths) = ctx.settings_and_paths_snapshot();
        state.apply_runtime_settings(&settings);
        state
            .installed_addons
            .set_watch_gmod_dir(paths.gmod_dir.clone());
        let label = destination_select::destination_label(&settings, &paths);
        state
            .destination_select
            .reset_from_snapshot(destination_select::SettingsSnapshot::new(
                settings.clone(),
                paths,
            ));
        state.downloader.set_destination_label(label);
        let mut app = Self {
            ctx,
            thumbnails,
            state,
            window_id: None,
            library_warm_kicked: false,
            #[cfg(feature = "asset-studio")]
            audio_playback: None,
        };
        #[cfg(target_os = "macos")]
        app.install_macos_menu();

        // Seed placeholders from the persisted metadata snapshot so second-launch
        // grids paint blurred-then-sharp instead of blank while decoding.
        app.thumbnails.seed_thumbhashes(app.ctx.thumbhash_seed());

        app
    }

    fn startup_tasks(&mut self) -> Task<RootMessage> {
        let startup_snapshot_task =
            self.ctx
                .library_snapshot()
                .map_or_else(Task::none, |snapshot| {
                    #[cfg(feature = "debug")]
                    let snapshot = self.visible_library_snapshot(&snapshot);
                    let installed = self.apply_installed_addons_message(
                        installed_addons::Message::SnapshotPushed(
                            LibraryRefreshReason::Startup,
                            Ok(installed_addons::rows_from_snapshot(&snapshot)),
                        ),
                    );
                    let analyzer =
                        self.apply_size_analyzer_message(size_analyzer::Message::SnapshotPushed(
                            LibraryRefreshReason::Startup,
                            Ok(Some(snapshot.clone())),
                        ));
                    sync_search_installed_addons(&self.ctx.backend().search, Some(&snapshot));
                    Task::batch([installed, analyzer])
                });
        let startup_route = self.state.shell.route();
        let startup_route_task = self.route_lifecycle_task(startup_route, RouteLifecycle::Entered);
        let startup_library_started = self.apply_installed_addons_message(
            installed_addons::Message::LibraryRefreshStarted(LibraryRefreshReason::Startup),
        );
        let startup_library_task = self
            .ctx
            .begin_library_refresh(LibraryRefreshReason::Startup)
            .map_or_else(Task::none, |task| {
                task.map(|result| {
                    RootMessage::LibraryRefreshed(LibraryRefreshReason::Startup, result)
                })
            });
        // Connectivity is a level, not an edge: the backend initializes before
        // the Iced app is constructed, so on fast connects the SteamConnected
        // event fires before our event sink exists and is lost. Seed the
        // session from the current level; the session state machine dedups a
        // repeated Connected if the live event also arrives.
        let steam_bootstrap_task = if self.ctx.steam_connected() {
            Task::done(RootMessage::SteamSession(
                steam_session::Message::ConnectionEvent(steam_session::ConnectionEvent::Connected),
            ))
        } else {
            Task::none()
        };
        let update_check_task = self
            .ctx
            .run_blocking("shell-update-check", |_app| {
                shell::fetch_latest_update(env!("CARGO_PKG_VERSION")).map_err(Arc::new)
            })
            .map(RootMessage::UpdateCheckCompleted);
        // The analyzer rasterizes its category labels synchronously in
        // update(); loading the font database is the one expensive part, so
        // warm the shared context off the UI thread before the route is
        // first entered. One-shot — no timer.
        let label_context_warmup_task = self
            .ctx
            .run_blocking("label-context-warmup", |_app| {
                crate::media::size_analyzer_render::with_shared_label_context(|_context| ());
            })
            .discard();

        Task::batch([
            system::theme().map(RootMessage::SystemThemeObserved),
            startup_snapshot_task,
            startup_route_task,
            startup_library_started,
            startup_library_task,
            steam_bootstrap_task,
            update_check_task,
            label_context_warmup_task,
        ])
    }

    pub(crate) fn update(&mut self, message: RootMessage) -> Task<RootMessage> {
        match message {
            RootMessage::Shell(message) => self.apply_shell_message(message),
            RootMessage::MyWorkshop(message) => self.apply_my_workshop_message(message),
            RootMessage::InstalledAddons(message) => self.apply_installed_addons_message(message),
            RootMessage::Downloader(message) => self.apply_downloader_message(message),
            RootMessage::SizeAnalyzer(message) => self.apply_size_analyzer_message(message),
            RootMessage::Search(message) => self.apply_search_message(message),
            RootMessage::DestinationSelect(message) => {
                self.apply_destination_select_message(message)
            }
            RootMessage::FilePreview(message) => self.apply_file_preview_message(message),
            #[cfg(feature = "asset-studio")]
            RootMessage::PreparePublish(prepare_publish::Message::FilePreview(message)) => {
                self.apply_file_preview_message(message)
            }
            RootMessage::PreparePublish(message) => self.prepare_publish_message_task(&message),
            #[cfg(feature = "asset-studio")]
            RootMessage::PreviewGma(preview_gma::Message::FilePreview(message)) => {
                self.apply_file_preview_message(message)
            }
            RootMessage::PreviewGma(message) => self.apply_preview_gma_message(message),
            RootMessage::Settings(message) => self.apply_settings_message(message),
            RootMessage::ContextMenu(message) => self.apply_context_menu_message(message),
            RootMessage::SteamSession(message) => self.apply_steam_session_message(message),
            RootMessage::ModalStack(message) => self.apply_modal_stack_message(&message),
            RootMessage::SystemThemeObserved(mode) => {
                self.state.apply_system_theme(mode);
                Task::none()
            }
            RootMessage::DragRegionPressed => self.window_drag_task(),
            RootMessage::DragRegionDoubleClicked => self.window_toggle_maximize_task(),
            RootMessage::TaskEvent(event) => {
                let overlay = self.apply_tasks_overlay_message(
                    tasks_overlay::Message::TaskEventsReceived(vec![event.clone()]),
                );
                let downloader = self
                    .apply_downloader_message(downloader::Message::TaskEventsReceived(vec![event]));
                Task::batch([overlay, downloader])
            }
            RootMessage::TasksOverlay(message) => self.apply_tasks_overlay_message(message),
            RootMessage::BackendEvent(event) => self.backend_event_task(event),
            RootMessage::LibraryWatch(message) => match message {
                library_watch::Message::WatchArmed { degraded } => self
                    .apply_installed_addons_message(installed_addons::Message::WatchArmed {
                        degraded,
                    }),
                library_watch::Message::DiskChanged => {
                    self.request_library_refresh(LibraryRefreshReason::DiskChanged)
                }
            },
            RootMessage::LibraryRefreshRequested(reason) => self.request_library_refresh(reason),
            RootMessage::LibraryRefreshed(reason, result) => {
                self.library_refreshed_task(reason, result)
            }
            RootMessage::UpdateCheckCompleted(result) => match result {
                Ok(Ok(Some(release))) => {
                    self.apply_shell_message(shell::Message::UpdateReleaseFound(release))
                }
                Ok(Ok(None)) => Task::none(),
                Ok(Err(error)) => {
                    log::debug!("update check failed: {error}");
                    Task::none()
                }
                Err(error) => {
                    log::warn!("failed to schedule update check: {error}");
                    Task::none()
                }
            },
            RootMessage::ThumbnailDemand(thumbnail_demand::Message::Delivered(delivery)) => {
                log_thumbnail_delivery(&delivery);
                if let thumbnail_demand::DeliveryResult::Ready(ready) = &delivery.result
                    && let (Some(url), Some(hash)) = (delivery.key.source_url(), ready.thumbhash())
                {
                    self.ctx.record_thumbhash(url, hash);
                }
                let _updated = self
                    .state
                    .installed_addons
                    .apply_thumbnail_delivery(&delivery);
                let _updated = self.state.my_workshop.apply_thumbnail_delivery(&delivery);
                let well = self.state.tokens.colors.surface_sunken;
                let _updated = self
                    .state
                    .prepare_publish
                    .apply_thumbnail_delivery(&delivery, [well.r, well.g, well.b]);
                let _updated = self.state.preview_gma.apply_thumbnail_delivery(&delivery);
                let _updated = self.state.search.apply_thumbnail_delivery(&delivery);
                let _invalidation = self.state.size_analyzer.apply_thumbnail_delivery(&delivery);
                Task::none()
            }
            RootMessage::WarmLibraryResolved(preview_urls) => {
                self.warm_library_demands_task(preview_urls)
            }
            RootMessage::ThumbnailDemand(message) => self
                .thumbnails
                .update(&self.ctx, message)
                .map(RootMessage::ThumbnailDemand),
            RootMessage::WindowEvent(id, event) => self.window_event_task(id, &event),
            RootMessage::WindowScaleFactorObserved(id, scale_factor) => {
                self.apply_window_scale_factor(id, scale_factor)
            }
            RootMessage::AddonDrag(message) => self.addon_drag_event_task(&message),
            RootMessage::LayoutObserved(size) => {
                self.state.viewport_size = size;
                Task::none()
            }
            RootMessage::GlobalShortcut(shortcut) => {
                // ⌘, is a toggle: it alone passes the modal gate, and only
                // while Settings itself is the top layer, so it can close
                // what it opened.
                if matches!(shortcut, GlobalShortcut::ToggleSettings)
                    && self.settings_shortcut_should_close()
                {
                    return Task::done(RootMessage::Settings(settings::Message::CloseRequested));
                }
                if !self.global_shortcuts_enabled() {
                    return Task::none();
                }

                match shortcut {
                    GlobalShortcut::ToggleSearch => self.toggle_search_palette_task(),
                    #[cfg(feature = "asset-studio")]
                    GlobalShortcut::ToggleFileSearch => self.toggle_file_search_palette_task(),
                    GlobalShortcut::ToggleSettings => {
                        Task::batch([self.dismiss_account_menu_task(), self.settings_open_task()])
                    }
                    GlobalShortcut::NavigateRoute(route) => {
                        Task::done(RootMessage::Shell(shell::Message::Navigate(route)))
                    }
                }
            }
            #[cfg(target_os = "macos")]
            RootMessage::Menu(command) => self.menu_command_task(command),
            #[cfg(target_os = "macos")]
            RootMessage::MenuOpenGmaCompleted(path) => path.map_or_else(Task::none, |path| {
                Task::done(RootMessage::FileDropped(path))
            }),
            #[cfg(target_os = "macos")]
            RootMessage::SystemAppearanceChanged => self
                .window_id
                .map_or_else(Task::none, |id| self.traffic_light_position_task(id)),
            RootMessage::FileDropped(path) => self.handle_file_drop(path),
            #[cfg(target_os = "macos")]
            RootMessage::GmaDocumentsOpened(paths) => self.gma_documents_opened_task(paths),
            RootMessage::AnimationTick(now) => {
                self.state.context_menu.tick(now);
                self.state.shell.tick_motion(now);
                let _changed = self.state.tasks_overlay.tick(now);
                let palette_close_settled = self.state.search.tick_motion(now);
                self.state.prepare_publish.tick_browser_select_hover(now);
                self.state.downloader.tick_progress(now);
                let mut finish_tasks = Vec::new();
                if palette_close_settled {
                    // The fading rows kept their thumbnails demanded; release
                    // them after the palette has fully reset.
                    finish_tasks.push(self.search_thumbnail_demands());
                }
                for modal in self.state.modal_stack.tick(now) {
                    finish_tasks.push(self.finish_modal_close_task(modal));
                }
                Task::batch(finish_tasks)
            }
        }
    }

    /// Runs each effect through `run` and batches the resulting tasks. The
    /// single generic replaces a `run_x_effects` wrapper per feature module;
    /// each item still needs its own `&mut self`, so the intermediate
    /// `Vec<Task<_>>` is unavoidable.
    fn batch_effects<E>(
        &mut self,
        effects: Vec<E>,
        mut run: impl FnMut(&mut Self, E) -> Task<RootMessage>,
    ) -> Task<RootMessage> {
        let mut tasks = Vec::with_capacity(effects.len());
        for effect in effects {
            tasks.push(run(self, effect));
        }
        Task::batch(tasks)
    }

    fn apply_tasks_overlay_message(
        &mut self,
        message: tasks_overlay::Message,
    ) -> Task<RootMessage> {
        let effects = tasks_overlay::update(&mut self.state.tasks_overlay, message);
        self.batch_effects(effects, |app, effect| match effect {
            tasks_overlay::Effect::CancelRequested(task_id) => {
                // Uncorrelated tasks have nothing to cancel; the press is a
                // no-op and the toast settles when its work does.
                let _cancelled = app.ctx.cancel_task(task_id);
                Task::none()
            }
        })
    }

    fn apply_shell_message(&mut self, message: shell::Message) -> Task<RootMessage> {
        let effects = shell::update(
            &mut self.state.shell,
            message,
            &self.state.tokens,
            self.state.chrome_strategy,
        );
        self.batch_effects(effects, Self::run_shell_effect)
    }

    #[cfg(target_os = "macos")]
    fn install_macos_menu(&self) {
        #[cfg(not(test))]
        platform_menu::install(&self.state.i18n);
    }

    #[cfg(target_os = "macos")]
    fn menu_command_task(&self, command: platform_menu::Command) -> Task<RootMessage> {
        match command {
            platform_menu::Command::Settings => {
                Task::done(RootMessage::Shell(shell::Message::SettingsActivated))
            }
            platform_menu::Command::OpenGma => self.menu_open_gma_task(),
            platform_menu::Command::Navigate(route) => {
                Task::done(RootMessage::Shell(shell::Message::Navigate(route)))
            }
            platform_menu::Command::OpenUrl(url) => self.open_url_task(url.to_owned()),
            platform_menu::Command::Unknown(id) => {
                log::debug!("ignored unknown macOS menu item {id:?}");
                Task::none()
            }
        }
    }

    fn apply_downloader_message(&mut self, message: downloader::Message) -> Task<RootMessage> {
        let effects = downloader::update(&mut self.state.downloader, message);
        self.batch_effects(effects, Self::run_downloader_effect)
    }

    fn apply_my_workshop_message(&mut self, message: my_workshop::Message) -> Task<RootMessage> {
        #[cfg(feature = "debug")]
        let message = self.filter_my_workshop_message(message);
        let effects = my_workshop::update(&mut self.state.my_workshop, message);
        self.batch_effects(effects, Self::run_my_workshop_effect)
    }

    #[cfg(feature = "debug")]
    fn filter_my_workshop_message(&self, message: my_workshop::Message) -> my_workshop::Message {
        match message {
            my_workshop::Message::PageCompleted(generation, page, Ok(mut result)) => {
                result.retain_visible(&self.state.hidden_workshop_ids);
                my_workshop::Message::PageCompleted(generation, page, Ok(result))
            }
            my_workshop::Message::StatsRefreshCompleted(generation, Ok(mut counts)) => {
                counts
                    .retain(|workshop_id, _| !self.state.hidden_workshop_ids.contains(workshop_id));
                my_workshop::Message::StatsRefreshCompleted(generation, Ok(counts))
            }
            message => message,
        }
    }

    #[cfg(feature = "debug")]
    fn visible_library_snapshot(&self, snapshot: &LibrarySnapshot) -> LibrarySnapshot {
        LibrarySnapshot {
            addons: snapshot
                .addons
                .iter()
                .filter(|addon| {
                    !self.state.hidden_addon_paths.contains(&addon.path)
                        && !addon.workshop_id.is_some_and(|workshop_id| {
                            self.state.hidden_workshop_ids.contains(&workshop_id)
                        })
                })
                .cloned()
                .collect::<Vec<_>>()
                .into(),
            epoch: snapshot.epoch,
        }
    }

    fn apply_installed_addons_message(
        &mut self,
        message: installed_addons::Message,
    ) -> Task<RootMessage> {
        let effects = installed_addons::update(&mut self.state.installed_addons, message);
        self.batch_effects(effects, Self::run_installed_addons_effect)
    }

    fn apply_size_analyzer_message(
        &mut self,
        message: size_analyzer::Message,
    ) -> Task<RootMessage> {
        let effects = size_analyzer::update(&mut self.state.size_analyzer, message);
        self.batch_effects(effects, Self::run_size_analyzer_effect)
    }

    fn apply_search_message(&mut self, message: search::Message) -> Task<RootMessage> {
        let effects = search::update(&mut self.state.search, message);
        self.batch_effects(effects, Self::run_search_effect)
    }

    fn apply_destination_select_message(
        &mut self,
        message: destination_select::Message,
    ) -> Task<RootMessage> {
        let effects = destination_select::update(&mut self.state.destination_select, message);
        self.batch_effects(effects, Self::run_destination_select_effect)
    }

    fn apply_preview_gma_message(&mut self, message: preview_gma::Message) -> Task<RootMessage> {
        let effects = preview_gma::update(&mut self.state.preview_gma, message);
        self.batch_effects(effects, Self::run_preview_gma_effect)
    }

    fn apply_settings_message(&mut self, message: settings::Message) -> Task<RootMessage> {
        let effects = settings::update(&mut self.state.settings, message);
        self.batch_effects(effects, Self::run_settings_effect)
    }

    fn run_shell_effect(&mut self, effect: shell::Effect) -> Task<RootMessage> {
        match effect {
            shell::Effect::OpenSettings => self.settings_open_task(),
            shell::Effect::OpenSearchPalette => self.open_search_palette_task(),
            shell::Effect::OpenUrl(url) => self.open_url_task(url),
            shell::Effect::Navigated { from, to } => self.shell_navigation_effect_task(from, to),
            shell::Effect::BeginWindowDrag => self.window_drag_task(),
            shell::Effect::ToggleMaximize => self.window_toggle_maximize_task(),
        }
    }

    fn run_downloader_effect(&mut self, effect: downloader::Effect) -> Task<RootMessage> {
        match effect {
            downloader::Effect::WorkshopSubmissionAccepted(item_ids) => {
                self.downloader_submission_task(item_ids)
            }
            downloader::Effect::TaskCancellationRequested(task_ids) => {
                for task_id in task_ids {
                    if !self.ctx.cancel_task(task_id) {
                        log::debug!(
                            "downloader cancellation for task {task_id:?} had no effect (already terminal or not yet correlated)"
                        );
                    }
                }
                Task::none()
            }
            downloader::Effect::DownloadQueueCancellationRequested => {
                self.ctx.cancel_all_workshop_downloads();
                Task::none()
            }
            downloader::Effect::PathsOpenRequested(paths) => self.downloader_open_paths_task(paths),
            downloader::Effect::PreviewRequested(target) => {
                self.apply_preview_gma_message(preview_gma::Message::OpenRequested(
                    preview_gma::OpenTarget::new(target.path, target.title, target.workshop_id),
                ))
            }
            downloader::Effect::WorkshopPageOpenRequested(workshop_id) => {
                let url = workshop_id.map_or_else(
                    || STEAM_WORKSHOP_URL.to_owned(),
                    |id| workshop_url::workshop_item_url(id.to_string()),
                );
                self.open_url_task(url)
            }
            downloader::Effect::BulkExtractPickerRequested => {
                self.downloader_bulk_extract_picker_task()
            }
            downloader::Effect::LocalExtractionRequested(paths) => {
                self.downloader_local_extraction_task(paths)
            }
            downloader::Effect::DestinationSelectionRequested => {
                let context = destination_select::OpenContext {
                    confirm_label_key: "destination-set-destination",
                    extracted_name: None,
                    force_create_folder: true,
                };
                self.destination_select_open_task(context)
            }
            downloader::Effect::WorkshopTitleQueryRequested(item_ids) => {
                self.downloader_title_query_task(item_ids)
            }
            downloader::Effect::ActiveJobCountChanged(count) => {
                self.apply_shell_message(shell::Message::DownloaderJobCountChanged(count))
            }
        }
    }

    fn run_my_workshop_effect(&mut self, effect: my_workshop::Effect) -> Task<RootMessage> {
        match effect {
            my_workshop::Effect::PageRequested { generation, page } => {
                self.my_workshop_page_task(generation, page)
            }
            my_workshop::Effect::StatsRefreshRequested { generation, pages } => {
                self.my_workshop_stats_refresh_task(generation, pages)
            }
            my_workshop::Effect::PreparePublishRequested(target) => {
                self.my_workshop_prepare_publish_task(target)
            }
            my_workshop::Effect::ContextMenuRequested(menu) => {
                self.my_workshop_context_menu_task(menu)
            }
            my_workshop::Effect::ThumbnailDemandsChanged => self.my_workshop_thumbnail_demands(),
            my_workshop::Effect::AddonDragPressed {
                card_id,
                workshop_id,
            } => {
                let thumbnail = self.state.my_workshop.drag_thumbnail_for_card(&card_id);
                self.state.addon_drag.press(
                    AddonDragSource::MyWorkshop,
                    card_id,
                    workshop_id,
                    thumbnail,
                );
                Task::none()
            }
            my_workshop::Effect::AddonDragReleased => self.finish_addon_drag_task(),
        }
    }

    fn run_installed_addons_effect(
        &mut self,
        effect: installed_addons::Effect,
    ) -> Task<RootMessage> {
        match effect {
            installed_addons::Effect::MetadataRequested {
                generation,
                item_ids,
            } => self.installed_addons_metadata_task(generation, item_ids),
            installed_addons::Effect::MetadataRefreshRequested {
                generation,
                item_ids,
            } => self.installed_addons_metadata_refresh_task(generation, item_ids),
            installed_addons::Effect::PreviewRequested(target) => {
                self.installed_addons_preview_task(target)
            }
            installed_addons::Effect::ContextMenuRequested(menu) => {
                self.installed_addons_context_menu_task(menu)
            }
            installed_addons::Effect::ThumbnailDemandsChanged => {
                self.installed_addons_thumbnail_demands()
            }
            installed_addons::Effect::AddonDragPressed {
                card_id,
                workshop_id,
            } => {
                let thumbnail = self
                    .state
                    .installed_addons
                    .drag_thumbnail_for_card(&card_id);
                self.state.addon_drag.press(
                    AddonDragSource::InstalledAddons,
                    card_id,
                    workshop_id,
                    thumbnail,
                );
                Task::none()
            }
            installed_addons::Effect::AddonDragReleased => self.finish_addon_drag_task(),
        }
    }

    fn run_size_analyzer_effect(&mut self, effect: size_analyzer::Effect) -> Task<RootMessage> {
        match effect {
            size_analyzer::Effect::PreviewUrlsResolveRequested(ids) => {
                self.size_analyzer_preview_url_task(ids)
            }
            size_analyzer::Effect::PreviewRequested(target) => {
                self.size_analyzer_preview_task(&target)
            }
            size_analyzer::Effect::ContextMenuRequested(menu) => {
                self.size_analyzer_context_menu_task(&menu)
            }
            size_analyzer::Effect::ThumbnailDemandsChanged => {
                self.size_analyzer_thumbnail_demands()
            }
            size_analyzer::Effect::AddonDragPressed {
                card_id,
                workshop_id,
            } => {
                let thumbnail = workshop_id
                    .and_then(|id| self.state.size_analyzer.tile_for(id))
                    .map(|tile| tile.handle.clone());
                self.state.addon_drag.press(
                    AddonDragSource::SizeAnalyzer,
                    card_id,
                    workshop_id,
                    thumbnail,
                );
                Task::none()
            }
            size_analyzer::Effect::AddonDragReleased => self.finish_addon_drag_task(),
        }
    }

    fn run_search_effect(&mut self, effect: search::Effect) -> Task<RootMessage> {
        match effect {
            search::Effect::PaletteOpened => self.search_thumbnail_demands(),
            search::Effect::PaletteDismissed => self.search_thumbnail_demands(),
            search::Effect::FocusInputRequested => self.search_focus_input_task(),
            search::Effect::QuickSearchDebounceRequested(request) => {
                self.search_quick_debounce_task(request)
            }
            search::Effect::QuickSearchRequested(request) => self.search_quick_task(request),
            search::Effect::FullSearchRequested => self.search_full_task(),
            search::Effect::MetadataRefreshRequested {
                generation,
                item_ids,
            } => self.search_metadata_refresh_task(generation, item_ids),
            search::Effect::TaskCancellationRequested(task_id) => {
                if !self.ctx.cancel_task(task_id) {
                    log::debug!(
                        "search cancellation for task {task_id:?} had no effect (already terminal or not yet correlated)"
                    );
                }
                Task::none()
            }
            search::Effect::ResultActivated(row_id) => self.search_result_task(row_id),
            search::Effect::ThumbnailDemandsChanged => self.search_thumbnail_demands(),
        }
    }

    fn run_destination_select_effect(
        &mut self,
        effect: destination_select::Effect,
    ) -> Task<RootMessage> {
        match effect {
            destination_select::Effect::ModalOpenRequested => {
                self.open_modal_stack_task(modal_stack::ActiveModal::DestinationSelect)
            }
            destination_select::Effect::SnapshotApplied => self.sync_downloader_destination_label(),
            destination_select::Effect::FolderPickerRequested => self
                .destination_select_folder_picker_task(
                    self.state.destination_select.initial_browse_directory(),
                ),
            destination_select::Effect::CreateFolderChanged(enabled) => {
                self.destination_select_create_folder_task(enabled)
            }
            destination_select::Effect::DestinationPersistRequested(request) => {
                self.destination_select_save_task(request)
            }
            destination_select::Effect::DestinationPersisted => Task::batch([
                if self.state.modal_stack.overlay_active() {
                    self.close_modal_stack_task()
                } else {
                    Task::none()
                },
                self.sync_downloader_destination_label(),
                self.preview_gma_destination_persisted_task(),
                self.context_menu_destination_persisted_task(),
            ]),
            destination_select::Effect::DestinationDismissed => Task::batch([
                self.preview_gma_destination_dismissed_task(),
                self.context_menu_destination_dismissed_task(),
            ]),
        }
    }

    fn run_preview_gma_effect(&mut self, effect: preview_gma::Effect) -> Task<RootMessage> {
        match effect {
            preview_gma::Effect::ModalOpenRequested => {
                self.open_modal_stack_task(modal_stack::ActiveModal::PreviewGma)
            }
            preview_gma::Effect::ArchiveOpenRequested(request) => {
                self.preview_gma_open_archive_task(request)
            }
            preview_gma::Effect::WorkshopMetadataRequested(request) => {
                self.preview_gma_workshop_metadata_task(&request)
            }
            preview_gma::Effect::AuthorFetchRequested(request) => {
                self.preview_gma_author_task(&request)
            }
            preview_gma::Effect::DestinationSelectRequested => {
                let extracted_name = self
                    .state
                    .preview_gma
                    .archive()
                    .map(|archive| archive.extracted_name().to_owned())
                    .filter(|name| !name.is_empty());
                let context = destination_select::OpenContext {
                    confirm_label_key: "destination-extract",
                    extracted_name,
                    force_create_folder: false,
                };
                self.destination_select_open_task(context)
            }
            #[cfg(not(feature = "asset-studio"))]
            preview_gma::Effect::EntryExtractionRequested(request) => {
                self.preview_gma_entry_extraction_task(request)
            }
            #[cfg(feature = "asset-studio")]
            preview_gma::Effect::EntryPreviewRequested(request) => {
                self.apply_file_preview_message(file_preview::Message::OpenRequested(request))
            }
            preview_gma::Effect::OpenUrlRequested(url) => self.open_url_task(url),
            preview_gma::Effect::CopyTextRequested(text) => self.copy_text_task(text),
            preview_gma::Effect::RevealPathRequested(path) => self.reveal_path_task(path),
            preview_gma::Effect::BrowserPathChanged => self.preview_gma_nav_autoscroll_task(),
            preview_gma::Effect::ThumbnailDemandsChanged => self.preview_gma_thumbnail_demands(),
        }
    }

    fn run_settings_effect(&mut self, effect: settings::Effect) -> Task<RootMessage> {
        match effect {
            settings::Effect::ModalOpenRequested => {
                self.open_modal_stack_task(modal_stack::ActiveModal::Settings)
            }
            settings::Effect::ModalCloseRequested => self.close_modal_stack_task(),
            settings::Effect::PathBrowseRequested(kind) => self.settings_folder_picker_task(kind),
            settings::Effect::PathValidationRequested(request) => {
                self.settings_path_validation_task(request)
            }
            settings::Effect::MutationApplied(mutation) => {
                let runtime_task = self.apply_settings_mutation_runtime(&mutation);
                let save_task = self.settings_save_task(mutation);
                Task::batch([runtime_task, save_task])
            }
            settings::Effect::SnapshotApplied(snapshot) => {
                self.apply_settings_snapshot_runtime(&snapshot)
            }
            settings::Effect::ResetRunRequested(action) => self.settings_reset_task(action),
        }
    }

    fn shell_navigation_effect_task(
        &mut self,
        from: shell::Route,
        to: shell::Route,
    ) -> Task<RootMessage> {
        debug_assert_ne!(
            from, to,
            "shell::Effect::Navigated is emitted only when the route changes"
        );
        self.route_transitioned_task(from, to)
    }

    pub(crate) fn view(&self) -> Element<'_, RootMessage> {
        let tokens = self.state.tokens;
        let ctx = theme::ViewCtx::new(&self.state.tokens, &self.state.i18n);
        let now = Instant::now();
        let route_body: Element<'_, RootMessage> = container(self.active_route_view())
            .padding(tokens.spacing.pad)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| theme::styles::surface(&tokens))
            .into();
        let body: Element<'_, RootMessage> = if self.state.chrome_strategy.mac_native_inset() {
            column![content_drag_strip(&tokens), route_body]
                .spacing(0.0)
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        } else {
            route_body
        };

        let content_area = row![
            shell::sidebar(
                &self.state.shell,
                ctx,
                self.state.chrome_strategy,
                self.state.addon_drag.is_dragging(),
                now,
            )
            .map(RootMessage::Shell),
            body,
        ]
        .spacing(0.0)
        .height(Length::Fill);

        let base_content: Element<'_, RootMessage> = container(content_area)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| theme::styles::surface(&tokens))
            .into();

        let base: Element<'_, RootMessage> = sensor(base_content)
            .on_show(RootMessage::LayoutObserved)
            .on_resize(RootMessage::LayoutObserved)
            .into();

        let mut layers = stack![base].width(Length::Fill).height(Length::Fill);
        // Cursor-only layers. The drag overlay must exclude the sidebar: a
        // layer with a cursor interaction levitates the cursor for everything
        // below it, which would blind the Downloader drop-target hover.
        if self.state.addon_drag.is_dragging() {
            layers = layers.push(addon_drag_cursor_overlay(&self.state, &tokens, now));
        }
        if let Some(dropdown) =
            search::dropdown_overlay(&self.state.search, ctx, self.state.viewport_size, now)
        {
            layers = layers.push(dropdown.map(RootMessage::Search));
        }

        if let Some(account_menu) = shell::account_menu_overlay(&self.state.shell, ctx, now) {
            layers = layers.push(account_menu.map(RootMessage::Shell));
        }

        if self.state.modal_stack.active().is_some() {
            let modal_scale = self.state.modal_stack.scale(now);
            let modal_interactive = self.state.modal_stack.interactive();
            layers = layers.push(
                modal_stack::scrim(&self.state.modal_stack, &tokens, now)
                    .map(RootMessage::ModalStack),
            );

            let chrome_clearance = if self.state.chrome_strategy.mac_native_inset() {
                tokens.dims.sidebar_band_height
            } else {
                0.0
            };

            if self.state.modal_stack.active() == Some(modal_stack::ActiveModal::PreparePublish) {
                // An expanded preview leaves only a thin ring of app visible;
                // black that ring out so it reads as background, not glow.
                if file_preview::embedded_expanded(&self.state.file_preview) {
                    layers = layers.push(
                        modal_stack::expanded_scrim(&self.state.modal_stack, &tokens, now)
                            .map(RootMessage::ModalStack),
                    );
                }
                let content = prepare_publish::view(
                    &self.state.prepare_publish,
                    &self.state.file_preview,
                    ctx,
                    self.state.viewport_size,
                    chrome_clearance,
                    now,
                )
                .map(RootMessage::PreparePublish);
                layers = layers.push(modal_stack::frame(content, modal_scale, modal_interactive));
            }

            if self.state.modal_stack.active() == Some(modal_stack::ActiveModal::PreviewGma) {
                // An expanded preview leaves only a thin ring of app visible;
                // black that ring out so it reads as background, not glow.
                if file_preview::embedded_expanded(&self.state.file_preview) {
                    layers = layers.push(
                        modal_stack::expanded_scrim(&self.state.modal_stack, &tokens, now)
                            .map(RootMessage::ModalStack),
                    );
                }
                let content = preview_gma::view(
                    &self.state.preview_gma,
                    &self.state.file_preview,
                    ctx,
                    self.state.viewport_size,
                    chrome_clearance,
                )
                .map(RootMessage::PreviewGma);
                layers = layers.push(modal_stack::frame(content, modal_scale, modal_interactive));
            }

            if self.state.modal_stack.active() == Some(modal_stack::ActiveModal::Settings) {
                let content = settings::view(&self.state.settings, ctx, self.state.viewport_size)
                    .map(RootMessage::Settings);
                layers = layers.push(modal_stack::frame(content, modal_scale, modal_interactive));
            }
        }

        // Overlay modals layer on top of whatever base modal is open, behind
        // their own scrim.
        if let Some(overlay_modal) = self.state.modal_stack.overlay_modal() {
            let overlay_scale = self.state.modal_stack.overlay_scale(now);
            let overlay_interactive = self.state.modal_stack.overlay_interactive();
            layers = layers.push(
                modal_stack::overlay_scrim(&self.state.modal_stack, &tokens, now)
                    .map(RootMessage::ModalStack),
            );

            if overlay_modal == modal_stack::ActiveModal::DestinationSelect {
                let content = destination_select::view(
                    &self.state.destination_select,
                    ctx,
                    self.state.viewport_size,
                )
                .map(RootMessage::DestinationSelect);
                layers = layers.push(modal_stack::frame(
                    content,
                    overlay_scale,
                    overlay_interactive,
                ));
            }
        }

        if self.state.context_menu.visible() {
            layers = layers.push(
                context_menu::view(&self.state.context_menu, ctx, self.state.viewport_size, now)
                    .map(RootMessage::ContextMenu),
            );
        }

        if self.state.addon_drag.is_dragging() {
            layers = layers.push(addon_drag_ghost(&self.state.addon_drag, &tokens));
        }

        // The tasks overlay is the topmost layer, above modal scrims, so
        // publish/extract progress stays visible whatever else is open.
        if let Some(overlay) = tasks_overlay::view(
            &self.state.tasks_overlay,
            ctx,
            self.state.viewport_size,
            now,
        ) {
            layers = layers.push(overlay.map(RootMessage::TasksOverlay));
        }

        // The wrapper intercepts ⌘F/⌘, ahead of every widget so a focused
        // palette input can never swallow the keystroke as text. While
        // Settings is the top layer only ⌘, stays live (to toggle it
        // closed); every other keystroke belongs to the modal's widgets.
        let shortcuts = if self.global_shortcuts_enabled() {
            Some(map_global_shortcut as _)
        } else if self.settings_shortcut_should_close() {
            Some(map_settings_toggle_shortcut as _)
        } else {
            None
        };
        shortcut_capture(layers, shortcuts).into()
    }

    fn active_route_view(&self) -> Element<'_, RootMessage> {
        let ctx = theme::ViewCtx::new(&self.state.tokens, &self.state.i18n);
        match self.state.shell.route() {
            shell::Route::MyWorkshop => {
                my_workshop::view(&self.state.my_workshop, ctx).map(RootMessage::MyWorkshop)
            }
            shell::Route::InstalledAddons => {
                installed_addons::view(&self.state.installed_addons, ctx)
                    .map(RootMessage::InstalledAddons)
            }
            shell::Route::Downloader => {
                downloader::view(&self.state.downloader, ctx).map(RootMessage::Downloader)
            }
            shell::Route::SizeAnalyzer => {
                size_analyzer::view(&self.state.size_analyzer, ctx).map(RootMessage::SizeAnalyzer)
            }
        }
    }

    fn request_library_refresh(&mut self, reason: LibraryRefreshReason) -> Task<RootMessage> {
        let started = self.apply_installed_addons_message(
            installed_addons::Message::LibraryRefreshStarted(reason),
        );
        let refresh = self
            .ctx
            .begin_library_refresh(reason)
            .map_or_else(Task::none, |task| {
                task.map(move |result| RootMessage::LibraryRefreshed(reason, result))
            });

        Task::batch([started, refresh])
    }

    fn library_refreshed_task(
        &mut self,
        requested_reason: LibraryRefreshReason,
        result: Result<LibraryRefresh, RunBlockingError>,
    ) -> Task<RootMessage> {
        let refresh = match result {
            Ok(refresh) => refresh,
            Err(error) => {
                log::warn!("library refresh failed to run: {error}");
                let error = UiError::from(&error);
                let installed = self.apply_installed_addons_message(
                    installed_addons::Message::SnapshotPushed(requested_reason, Err(error.clone())),
                );
                let analyzer = self.apply_size_analyzer_message(
                    size_analyzer::Message::SnapshotPushed(requested_reason, Err(error)),
                );
                let rerun = self
                    .ctx
                    .abort_library_refresh()
                    .map_or_else(Task::none, |reason| self.request_library_refresh(reason));
                return Task::batch([installed, analyzer, rerun]);
            }
        };

        #[cfg(feature = "debug")]
        let visible_snapshot = refresh
            .snapshot
            .as_ref()
            .map(|snapshot| self.visible_library_snapshot(snapshot));
        #[cfg(not(feature = "debug"))]
        let visible_snapshot = refresh.snapshot.clone();

        sync_search_installed_addons(&self.ctx.backend().search, visible_snapshot.as_ref());

        let warm_library = visible_snapshot
            .as_ref()
            .map_or_else(Task::none, |snapshot| self.warm_library_kick_task(snapshot));

        let rows = visible_snapshot.as_ref().map_or_else(
            || {
                Err(UiError::new(
                    gmpublished_backend::error_key::keys::GMOD_PATH_MISSING,
                ))
            },
            |snapshot| Ok(installed_addons::rows_from_snapshot(snapshot)),
        );
        let installed = self.apply_installed_addons_message(
            installed_addons::Message::SnapshotPushed(refresh.reason, rows),
        );
        let analyzer = self.apply_size_analyzer_message(size_analyzer::Message::SnapshotPushed(
            refresh.reason,
            Ok(visible_snapshot),
        ));

        let rerun = refresh
            .rerun_after
            .map_or_else(Task::none, |reason| self.request_library_refresh(reason));
        let warm_connect = self.warm_steam_connect_task();
        Task::batch([installed, analyzer, rerun, warm_connect, warm_library])
    }

    fn open_search_palette_task(&mut self) -> Task<RootMessage> {
        self.open_search_mode_palette_task(SearchMode::Addons)
    }

    fn open_search_mode_palette_task(&mut self, mode: SearchMode) -> Task<RootMessage> {
        Task::batch([
            self.dismiss_account_menu_task(),
            self.apply_search_message(search::Message::ModeFocusRequested(mode)),
        ])
    }

    fn toggle_search_palette_task(&mut self) -> Task<RootMessage> {
        if self.state.search.palette_open() && self.state.search.mode() == SearchMode::Addons {
            self.apply_search_message(search::Message::DismissRequested)
        } else {
            self.open_search_palette_task()
        }
    }

    #[cfg(feature = "asset-studio")]
    fn toggle_file_search_palette_task(&mut self) -> Task<RootMessage> {
        if self.state.search.palette_open() && self.state.search.mode() == SearchMode::Files {
            self.apply_search_message(search::Message::DismissRequested)
        } else {
            self.open_search_mode_palette_task(SearchMode::Files)
        }
    }

    fn dismiss_account_menu_task(&mut self) -> Task<RootMessage> {
        self.apply_shell_message(shell::Message::AccountMenuDismissed)
    }

    fn dismiss_search_palette_task(&mut self) -> Task<RootMessage> {
        if !self.state.search.palette_open() {
            return Task::none();
        }

        self.apply_search_message(search::Message::DismissRequested)
    }

    fn global_shortcuts_enabled(&self) -> bool {
        self.state.modal_stack.active().is_none()
            && !self.state.modal_stack.overlay_active()
            && !self.state.context_menu.visible()
    }

    /// ⌘, may close Settings only while it is the top layer; an overlay or
    /// context menu above it keeps the toggle inert like every other global
    /// shortcut, and a different active modal is never closed by it.
    fn settings_shortcut_should_close(&self) -> bool {
        self.state.modal_stack.active() == Some(modal_stack::ActiveModal::Settings)
            && !self.state.modal_stack.overlay_active()
            && !self.state.context_menu.visible()
    }

    pub(crate) fn subscription(&self) -> Subscription<RootMessage> {
        let mut streams = vec![self.ctx.task_events().map(RootMessage::TaskEvent)];
        streams.push(self.ctx.backend_events().map(RootMessage::BackendEvent));
        streams.push(shell::subscription(&self.state.shell).map(RootMessage::Shell));
        if self.state.follows_system_theme() {
            streams.push(system::theme_changes().map(RootMessage::SystemThemeObserved));
        }
        streams.push(
            theme::motion::redraw_subscription(self.state.needs_motion_ticks())
                .map(RootMessage::AnimationTick),
        );
        streams
            .push(my_workshop::subscription(&self.state.my_workshop).map(RootMessage::MyWorkshop));
        streams.push(
            library_watch::subscription(
                self.state
                    .installed_addons
                    .watch_gmod_dir()
                    .map(PathBuf::as_path),
                self.state.installed_addons.watch_arm_epoch(),
            )
            .map(RootMessage::LibraryWatch),
        );
        streams.push(
            installed_addons::subscription(&self.state.installed_addons)
                .map(RootMessage::InstalledAddons),
        );
        streams.push(
            prepare_publish::subscription(&self.state.prepare_publish)
                .map(RootMessage::PreparePublish),
        );
        streams
            .push(preview_gma::subscription(&self.state.preview_gma).map(RootMessage::PreviewGma));
        streams.push(
            file_preview::subscription(&self.state.file_preview).map(RootMessage::FilePreview),
        );
        streams.push(search::subscription(&self.state.search).map(RootMessage::Search));
        streams.push(settings::subscription(&self.state.settings).map(RootMessage::Settings));
        streams.push(
            context_menu::subscription(&self.state.context_menu).map(RootMessage::ContextMenu),
        );
        // Global shortcuts (⌘F/⌘,) are handled by the shortcut_capture
        // wrapper in view(), not a subscription: subscriptions observe key
        // events after the widget tree, which let a focused palette input
        // swallow the closing ⌘F as text for a frame.
        // The context menu owns Escape while it is visible; the modal keeps
        // its scrim-click dismissal in the meantime.
        if !self.state.context_menu.visible() {
            streams.push(
                modal_stack::subscription(&self.state.modal_stack).map(RootMessage::ModalStack),
            );
        }
        streams.push(window::events().map(|(id, event)| RootMessage::WindowEvent(id, event)));
        if self.state.addon_drag.is_active() {
            streams.push(event::listen_with(addon_drag_event));
        }
        // No continuous cursor-position stream: the Size Analyzer tooltip
        // anchors to the hovered square and redraws only on hover enter/leave,
        // and context menus capture their position at press time. Pointer
        // movement stays out of the message loop entirely (idle-0%).
        streams.push(event::listen_with(file_drop_event));
        #[cfg(target_os = "macos")]
        streams.push(crate::platform_menu::subscription().map(RootMessage::Menu));
        #[cfg(target_os = "macos")]
        streams.push(
            crate::platform_chrome::appearance_change_subscription()
                .map(|()| RootMessage::SystemAppearanceChanged),
        );
        #[cfg(target_os = "macos")]
        streams.push(crate::platform_open::subscription().map(RootMessage::GmaDocumentsOpened));
        Subscription::batch(streams)
    }

    pub(crate) fn theme(&self) -> Option<Theme> {
        None
    }

    pub(crate) fn title(&self) -> String {
        self.state.i18n.tr(self.state.title)
    }
}

fn content_drag_strip(tokens: &Tokens) -> Element<'static, RootMessage> {
    let tokens = *tokens;
    mouse_area(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(tokens.dims.sidebar_band_height))
            .style(move |_| theme::styles::surface(&tokens)),
    )
    .on_press(RootMessage::DragRegionPressed)
    .on_double_click(RootMessage::DragRegionDoubleClicked)
    .into()
}

fn addon_drag_cursor_overlay(
    state: &State,
    tokens: &Tokens,
    _now: Instant,
) -> Element<'static, RootMessage> {
    let left = shell::sidebar_width(tokens, state.chrome_strategy);

    container(
        mouse_area(Space::new().width(Length::Fill).height(Length::Fill))
            .interaction(mouse::Interaction::Grabbing),
    )
    .padding(Padding {
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
        left,
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// Mapper installed while Settings is the top modal layer: ⌘, alone is
/// intercepted so it can toggle Settings closed; everything else passes
/// through to the modal's widgets.
fn map_settings_toggle_shortcut(
    key: &keyboard::Key,
    modifiers: keyboard::Modifiers,
) -> Option<RootMessage> {
    if !modifiers.command() || modifiers.shift() || modifiers.alt() {
        return None;
    }

    matches!(key.as_ref(), keyboard::Key::Character(","))
        .then_some(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSettings))
}

fn map_global_shortcut(key: &keyboard::Key, modifiers: keyboard::Modifiers) -> Option<RootMessage> {
    if !modifiers.command() || modifiers.shift() || modifiers.alt() {
        return None;
    }

    match key.as_ref() {
        keyboard::Key::Character(key) if key.eq_ignore_ascii_case("f") => {
            Some(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSearch))
        }
        #[cfg(feature = "asset-studio")]
        keyboard::Key::Character(key) if key.eq_ignore_ascii_case("k") => Some(
            RootMessage::GlobalShortcut(GlobalShortcut::ToggleFileSearch),
        ),
        keyboard::Key::Character(",") => {
            Some(RootMessage::GlobalShortcut(GlobalShortcut::ToggleSettings))
        }
        keyboard::Key::Character("1") => Some(RootMessage::GlobalShortcut(
            GlobalShortcut::NavigateRoute(shell::Route::MyWorkshop),
        )),
        keyboard::Key::Character("2") => Some(RootMessage::GlobalShortcut(
            GlobalShortcut::NavigateRoute(shell::Route::InstalledAddons),
        )),
        keyboard::Key::Character("3") => Some(RootMessage::GlobalShortcut(
            GlobalShortcut::NavigateRoute(shell::Route::Downloader),
        )),
        keyboard::Key::Character("4") => Some(RootMessage::GlobalShortcut(
            GlobalShortcut::NavigateRoute(shell::Route::SizeAnalyzer),
        )),
        _ => None,
    }
}
