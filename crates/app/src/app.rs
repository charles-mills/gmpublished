use std::{
    collections::{HashMap, VecDeque},
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

use crate::bridge::{
    AppPaths, DownloadCountFormat, Settings, SystemColorScheme, ThemePreset, UiDensity,
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
    context_menu, description_editor, destination_select, downloader, file_preview,
    installed_addons, modal_stack, my_workshop, prepare_publish, prerequisites, preview_gma,
    search, settings, shell, size_analyzer, steam_session, tasks_overlay,
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

mod display;
mod drag;
mod hidden_addons;
mod routes;
pub mod runners;
mod side_effects_addons;
mod side_effects_audio;
mod side_effects_description_editor;
mod side_effects_downloader;
mod side_effects_file_preview;
mod side_effects_prerequisites;
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
mod ui_tests;

use crate::media::audio_playback::AudioPlayback;
#[cfg(test)]
use context_menu::ContextMenuTarget;
#[cfg(test)]
use context_menu::LocalMenuTarget;
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
    backend_runtime_action_message, connect_steam_for_operation, flatten_blocking_ui_result,
    run_downloader_local_extraction, run_downloader_submission, run_installed_metadata_refresh,
    run_preview_gma_archive_extraction, run_preview_gma_entry_extraction, run_search_full,
    run_size_analyzer_preview_urls, schedule_native_open_target, send_root_message,
    spawn_blocking_detached_or_warn,
};
use side_effects_thumbnails::log_thumbnail_delivery;
use view_support::{addon_drag_ghost, resolve_tokens, system_scheme_from_mode};

#[derive(Debug)]
pub struct App {
    environment: AppEnvironment,
    state: State,
    window_id: Option<window::Id>,
    startup_phase: StartupPhase,
    library_warm_kicked: bool,
    audio_playback: Option<AudioPlayback>,
    update_context: UpdateContext,
    /// Only sanitize one snapshot at a time so an older job cannot overwrite newer settings.
    appdata_snapshot_in_flight: bool,
    pending_appdata_snapshot: Option<Box<gmpublished_backend::AppDataSnapshot>>,
    settings_save_in_flight: Option<(crate::generation::Generation, settings::SettingsMutation)>,
    settings_save_queue: VecDeque<(crate::generation::Generation, settings::SettingsMutation)>,
    pending_settings_reset: Option<settings::ResetAction>,
    settings_reset_in_flight: bool,
}

#[derive(Debug)]
pub struct AppEnvironment {
    ctx: BackendContext,
    thumbnails: thumbnail_demand::Manager,
    workshop_snapshot_sequence: std::cell::Cell<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StartupPhase {
    WaitingForFirstFrame,
    Started,
}

impl StartupPhase {
    fn activate_after_first_frame(&mut self) -> bool {
        if *self == Self::Started {
            return false;
        }
        *self = Self::Started;
        true
    }

    const fn started(self) -> bool {
        matches!(self, Self::Started)
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let cancelled = self.environment.ctx.shutdown();
        if cancelled > 0 {
            log::info!("cancelled {cancelled} in-flight transaction(s) on quit");
        }
    }
}

#[derive(Debug)]
pub struct State {
    features: FeatureStates,
    chrome_strategy: shell::ChromeStrategy,
    theme_preset: ThemePreset,
    download_count_format: DownloadCountFormat,
    density: UiDensity,
    density_motion: theme::motion::Presence<f32>,
    density_progress: f32,
    reduce_motion: bool,
    text_size_percent: u16,
    system_scheme: SystemColorScheme,
    accent_inputs: theme::AccentInputs,
    tokens: Tokens,
    addon_drag: AddonDragState,
    context_menu_extraction: ExtractionHandoff,
    hidden_addons: hidden_addons::HiddenAddons,
    viewport_size: Size,
    i18n: I18n,
}

#[derive(Debug, Default)]
pub struct FeatureStates {
    shell: shell::State,
    description_editor: description_editor::State,
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
    prerequisites: prerequisites::State,
    modal_stack: modal_stack::State,
    tasks_overlay: tasks_overlay::State,
}

impl Default for State {
    fn default() -> Self {
        let theme_preset = ThemePreset::default();
        let download_count_format = Settings::default().ui.download_count_format;
        let system_scheme = SystemColorScheme::Dark;
        let accent_inputs = theme::AccentInputs::for_preset(theme_preset);
        let mut state = Self {
            features: FeatureStates::default(),
            chrome_strategy: shell::ChromeStrategy::resolve(Settings::default().backend.titlebar),
            theme_preset,
            download_count_format,
            density: UiDensity::default(),
            density_motion: theme::motion::Presence::new(
                0.0,
                theme::invariant().motion.layout_duration(),
                iced::animation::Easing::EaseOut,
            ),
            density_progress: 0.0,
            reduce_motion: false,
            text_size_percent: 100,
            system_scheme,
            accent_inputs,
            tokens: resolve_tokens(theme_preset, system_scheme, accent_inputs)
                .with_display_preferences(UiDensity::default(), 100),
            addon_drag: AddonDragState::default(),
            context_menu_extraction: ExtractionHandoff::Idle,
            hidden_addons: hidden_addons::HiddenAddons::default(),
            viewport_size: Size::ZERO,
            i18n: I18n::from_user_or_system(None),
        };
        state.apply_localized_labels();
        state.initialize_ui_metrics();
        state
    }
}

impl State {
    fn follows_system_theme(&self) -> bool {
        self.theme_preset == ThemePreset::Auto
    }

    fn set_play_gifs_by_default(&mut self, enabled: bool) {
        self.features.my_workshop.set_play_gifs_by_default(enabled);
        self.features
            .installed_addons
            .set_play_gifs_by_default(enabled);
    }

    fn set_window_focused(&mut self, focused: bool) {
        self.features.my_workshop.set_window_focused(focused);
        self.features.installed_addons.set_window_focused(focused);
        self.features.prepare_publish.set_window_focused(focused);
        self.features.preview_gma.set_window_focused(focused);
        self.features.description_editor.set_window_focused(focused);
    }

    fn apply_thumbnail_delivery(&mut self, delivery: &thumbnail_demand::Delivery) {
        let _updated = self
            .features
            .installed_addons
            .apply_thumbnail_delivery(delivery);
        let _updated = self.features.my_workshop.apply_thumbnail_delivery(delivery);
        let well = self.tokens.colors.surface_sunken;
        let _updated = self
            .features
            .prepare_publish
            .apply_thumbnail_delivery(delivery, [well.r, well.g, well.b]);
        let _updated = self.features.preview_gma.apply_thumbnail_delivery(delivery);
        let _updated = self
            .features
            .description_editor
            .apply_thumbnail_delivery(delivery);
        let _updated = self.features.search.apply_thumbnail_delivery(delivery);
        let _invalidation = self
            .features
            .size_analyzer
            .apply_thumbnail_delivery(delivery);
    }

    fn invalidate_ready_thumbnails(&mut self) {
        let _changed = self.features.my_workshop.invalidate_ready_thumbnails();
        let _changed = self.features.installed_addons.invalidate_ready_thumbnails();
        let _changed = self.features.search.invalidate_ready_thumbnails();
        let _changed = self.features.preview_gma.invalidate_ready_thumbnail();
        let _changed = self
            .features
            .description_editor
            .invalidate_ready_thumbnails();
        let _invalidation = self.features.size_analyzer.invalidate_ready_thumbnails();
    }

    fn thumbnail_demand_sets(
        &self,
        search_viewport_height: f32,
    ) -> Vec<thumbnail_demand::DemandSet> {
        vec![
            self.features.my_workshop.thumbnail_demands(),
            self.features.installed_addons.thumbnail_demands(),
            self.features
                .search
                .thumbnail_demands(search_viewport_height),
            self.features.prepare_publish.thumbnail_demands(),
            self.features.preview_gma.thumbnail_demands(),
            self.features.description_editor.thumbnail_demands(),
            self.features.size_analyzer.thumbnail_demands(),
        ]
    }

    fn set_scale_factor(&mut self, scale_factor: f32) -> bool {
        self.features.size_analyzer.set_scale_factor(scale_factor)
    }

    fn apply_settings_snapshot(
        &mut self,
        settings: Settings,
        paths: AppPaths,
    ) -> SettingsFanoutResult {
        let previous_chrome = self.chrome_strategy;
        let previous_gmod_dir = self
            .features
            .installed_addons
            .watch_gmod_dir()
            .map(Path::to_path_buf);
        let destination_label = destination_select::destination_label(&settings, &paths);

        self.apply_runtime_settings(&settings);
        self.features
            .installed_addons
            .set_watch_gmod_dir(paths.gmod_dir.clone());
        let gmod_dir_changed = previous_gmod_dir != paths.gmod_dir;
        self.features
            .destination_select
            .reset_from_snapshot(destination_select::SettingsSnapshot::new(settings, paths));
        self.features
            .downloader
            .set_destination_label(destination_label);

        SettingsFanoutResult {
            previous_chrome,
            gmod_dir_changed,
        }
    }

    fn apply_runtime_settings(&mut self, settings: &Settings) {
        self.theme_preset = settings.ui.theme_preset;
        self.accent_inputs = settings::accent_inputs_from_settings(settings);
        self.reduce_motion = settings.ui.reduce_motion;
        self.transition_density(
            settings.ui.density,
            Instant::now(),
            self.viewport_size != Size::ZERO,
        );
        self.text_size_percent = settings.ui.text_size_percent;
        self.resolve_ui_tokens();
        self.chrome_strategy = shell::ChromeStrategy::resolve(settings.backend.titlebar);
        self.download_count_format = settings.ui.download_count_format;
        self.apply_runtime_language(settings.backend.language.as_deref());
        self.set_play_gifs_by_default(settings.ui.play_gifs_by_default);
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
        let publish_new = self.i18n.tr("publish-new");
        self.features.my_workshop.set_publish_new_title(publish_new);
    }

    fn download_count_formatter(&self) -> DownloadCountFormatter {
        DownloadCountFormatter::from_format_and_locale(
            self.download_count_format,
            Some(self.i18n.locale_id()),
        )
    }

    fn apply_download_count_formatter(&mut self) {
        let formatter = self.download_count_formatter();
        self.features
            .my_workshop
            .set_download_count_formatter(formatter);
        self.features
            .installed_addons
            .set_download_count_formatter(formatter);
        self.features
            .preview_gma
            .set_download_count_formatter(formatter);
    }

    fn apply_system_theme(&mut self, mode: Mode) {
        self.system_scheme = system_scheme_from_mode(mode);
        let system_scheme = self.system_scheme;
        self.features.settings.apply_system_scheme(system_scheme);

        if self.follows_system_theme() {
            self.resolve_ui_tokens();
        }
    }

    fn needs_motion_ticks(&self) -> bool {
        self.density_motion.needs_ticks()
            || self.features.modal_stack.needs_ticks()
            || self.features.context_menu.needs_ticks()
            || self.features.shell.needs_motion_ticks()
            || self.features.search.needs_motion_ticks()
            || self.features.tasks_overlay.needs_ticks()
            || self
                .features
                .prepare_publish
                .browser_select_hover_needs_ticks()
            || self.features.downloader.needs_progress_ticks()
            || self.features.prerequisites.animating(
                self.features.shell.route(),
                self.features.steam_session.status(),
            )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SettingsFanoutResult {
    previous_chrome: shell::ChromeStrategy,
    gmod_dir_changed: bool,
}

#[derive(Debug, Default, Eq, PartialEq)]
enum ExtractionHandoff {
    #[default]
    Idle,
    AwaitingDestination {
        paths: Vec<PathBuf>,
    },
}

impl ExtractionHandoff {
    fn begin(&mut self, paths: Vec<PathBuf>) {
        debug_assert!(!paths.is_empty(), "an extraction handoff needs input paths");
        *self = Self::AwaitingDestination { paths };
    }

    fn clear(&mut self) {
        *self = Self::Idle;
    }

    fn take_paths(&mut self) -> Option<Vec<PathBuf>> {
        match std::mem::take(self) {
            Self::Idle => None,
            Self::AwaitingDestination { paths } => Some(paths),
        }
    }

    #[cfg(test)]
    const fn paths(&self) -> Option<&[PathBuf]> {
        match self {
            Self::Idle => None,
            Self::AwaitingDestination { paths } => Some(paths.as_slice()),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct UpdateContext {
    now: Instant,
}

impl UpdateContext {
    fn capture() -> Self {
        Self {
            now: Instant::now(),
        }
    }

    fn for_message(message: &RootMessage) -> Self {
        let now = match message {
            RootMessage::AnimationTick(now) => *now,
            _ => return Self::capture(),
        };
        Self { now }
    }
}

#[derive(Clone, Debug)]
pub enum UpdateCheckOutcome {
    ReleaseAvailable(shell::UpdateRelease),
    UpToDate,
    CheckFailed(Arc<shell::UpdateCheckError>),
    WorkerFailed(RunBlockingError),
}

impl
    From<
        Result<
            Result<Option<shell::UpdateRelease>, Arc<shell::UpdateCheckError>>,
            RunBlockingError,
        >,
    > for UpdateCheckOutcome
{
    fn from(
        result: Result<
            Result<Option<shell::UpdateRelease>, Arc<shell::UpdateCheckError>>,
            RunBlockingError,
        >,
    ) -> Self {
        match result {
            Ok(Ok(Some(release))) => Self::ReleaseAvailable(release),
            Ok(Ok(None)) => Self::UpToDate,
            Ok(Err(error)) => Self::CheckFailed(error),
            Err(error) => Self::WorkerFailed(error),
        }
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
    DescriptionEditor(description_editor::Message),
    Settings(settings::Message),
    ContextMenu(context_menu::Message),
    SteamSession(steam_session::Message),
    Prerequisites(prerequisites::Message),
    SteamClientInstalledProbed(bool),
    ModalStack(modal_stack::Message),
    TasksOverlay(tasks_overlay::Message),
    SystemThemeObserved(Mode),
    DragRegionPressed,
    DragRegionDoubleClicked,
    TaskEvent(TaskEvent),
    BackendEvent(BackendRuntimeEvent),
    AppDataSnapshotApplied(Result<Box<settings::SettingsSnapshot>, RunBlockingError>),
    LibraryWatch(library_watch::Message),
    LibraryRefreshRequested(LibraryRefreshReason),
    LibraryRefreshed(
        LibraryRefreshReason,
        Result<LibraryRefresh, RunBlockingError>,
    ),
    UpdateCheckCompleted(UpdateCheckOutcome),
    ThumbnailDemand(thumbnail_demand::Message),
    WarmLibraryResolved(Vec<(PublishedFileId, String)>),
    FirstFramePresented(window::Id),
    WindowEvent(window::Id, window::Event),
    WindowScaleFactorObserved(window::Id, f32),
    AddonDrag(AddonDragMessage),
    LayoutObserved(Size),
    GlobalShortcut(GlobalShortcut),
    Platform(crate::platform::Message),
    FileDropped(PathBuf),
    Noop,
    AnimationTick(Instant),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GlobalShortcut {
    ToggleSearch,
    ToggleFileSearch,
    ToggleSettings,
    NavigateRoute(shell::Route),
}

fn sync_search_installed_addons(ctx: &BackendContext, snapshot: Option<&LibrarySnapshot>) {
    ctx.sync_installed_addon_search(
        snapshot.map_or_else(Vec::new, search_items_from_library),
        snapshot.map_or_else(Vec::new, search_file_items_from_library),
    );
}

#[allow(clippy::needless_pass_by_value)] // Required by iced's `listen_raw` callback signature.
fn first_frame_presented_event(
    event: Event,
    _status: event::Status,
    id: window::Id,
) -> Option<RootMessage> {
    match event {
        Event::Window(window::Event::RedrawRequested(_)) => {
            Some(RootMessage::FirstFramePresented(id))
        }
        _ => None,
    }
}

fn search_items_from_library(snapshot: &LibrarySnapshot) -> Vec<gmpublished_backend::SearchItem> {
    snapshot
        .addons
        .iter()
        .map(|addon| {
            let metadata = &addon.meta.header.metadata;
            let mut terms = metadata.tags().map(<[String]>::to_vec).unwrap_or_default();
            if let Some(addon_type) = metadata.addon_type() {
                terms.push(addon_type.to_owned());
            }
            if let Some(workshop_id) = addon.workshop_id {
                terms.push(workshop_id.get().to_string());
            }

            gmpublished_backend::SearchItem::new_installed_addon(
                addon.canonical_path.clone(),
                addon.workshop_id.map(PublishedFileId::get),
                addon.display_title(),
                terms,
                addon.modified_epoch_seconds,
            )
        })
        .collect()
}

fn search_file_items_from_library(
    snapshot: &LibrarySnapshot,
) -> Vec<gmpublished_backend::SearchItem> {
    let entry_count = snapshot
        .addons
        .iter()
        .map(|addon| addon.meta.entries.len())
        .sum();
    let mut items = Vec::with_capacity(entry_count);
    for addon in snapshot.addons.iter() {
        let shared = gmpublished_backend::FileSearchAddon::new(
            addon.canonical_path.clone(),
            addon.display_title(),
            addon.workshop_id.map(Into::into),
        );
        items.extend(addon.meta.entries.iter().map(move |entry| {
            gmpublished_backend::SearchItem::new_installed_addon_file(
                shared.clone(),
                entry.path.clone(),
                entry.size,
                entry.crc32,
                addon.modified_epoch_seconds,
            )
        }));
    }
    items
}

macro_rules! feature_dispatch {
    ($($method:ident => $feature:ident . $field:ident, $run:ident;)*) => {
        $(
            fn $method(&mut self, message: $feature::Message) -> Task<RootMessage> {
                let effects = $feature::update(&mut self.state.features.$field, message);
                self.batch_effects(effects, Self::$run)
            }
        )*
    };
}

impl App {
    #[cfg(test)]
    pub(crate) fn new_for_test() -> Self {
        Self::from_context(
            BackendContext::new().expect("test backend context"),
            StartupPhase::Started,
            thumbnail_demand::Config::default(),
        )
    }

    pub(crate) fn new(ctx: BackendContext) -> (Self, Task<RootMessage>) {
        (
            Self::from_context(
                ctx,
                StartupPhase::WaitingForFirstFrame,
                thumbnail_demand::Config {
                    disk_cache_dir: gmpublished_backend::cache_dir()
                        .map(|dir| dir.join("thumbnails")),
                    ..thumbnail_demand::Config::default()
                },
            ),
            Task::none(),
        )
    }

    /// Keep external work in `startup_tasks` so state tests cannot start network or discovery jobs.
    fn from_context(
        ctx: BackendContext,
        startup_phase: StartupPhase,
        thumbnail_config: thumbnail_demand::Config,
    ) -> Self {
        let thumbnails = thumbnail_demand::Manager::new(thumbnail_config);
        let mut state = State::default();
        let (settings, paths) = ctx.settings_and_paths_snapshot();
        let _result = state.apply_settings_snapshot(settings, paths);
        state.initialize_ui_metrics();
        ctx.set_workshop_language(gmpublished_backend::WorkshopLanguage::from_locale(
            state.i18n.locale_id(),
        ));
        let app = Self {
            environment: AppEnvironment {
                ctx,
                thumbnails,
                workshop_snapshot_sequence: std::cell::Cell::new(0),
            },
            state,
            window_id: None,
            startup_phase,
            library_warm_kicked: false,
            audio_playback: None,
            update_context: UpdateContext::capture(),
            appdata_snapshot_in_flight: false,
            pending_appdata_snapshot: None,
            settings_save_in_flight: None,
            settings_save_queue: VecDeque::new(),
            pending_settings_reset: None,
            settings_reset_in_flight: false,
        };
        #[cfg(target_os = "macos")]
        app.install_macos_menu();

        app
    }

    fn activate_startup_after_first_frame(&mut self) -> Task<RootMessage> {
        if !self.startup_phase.activate_after_first_frame() {
            return Task::none();
        }

        if let Err(error) = self.environment.ctx.activate_startup_services() {
            // Local browsing still works if Steam startup fails.
            log::error!("could not start backend background services: {error}");
        }
        self.environment
            .thumbnails
            .seed_thumbhashes(self.environment.ctx.thumbhash_seed());
        self.startup_tasks()
    }

    fn startup_tasks(&mut self) -> Task<RootMessage> {
        let startup_snapshot_task =
            self.environment
                .ctx
                .library_snapshot()
                .map_or_else(Task::none, |snapshot| {
                    let snapshot = self.state.hidden_addons.visible(&snapshot);
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
                    sync_search_installed_addons(&self.environment.ctx, Some(&snapshot));
                    Task::batch([installed, analyzer])
                });
        let startup_route = self.state.features.shell.route();
        let startup_route_task = self.route_lifecycle_task(startup_route, RouteLifecycle::Entered);
        let startup_library_started = self.apply_installed_addons_message(
            installed_addons::Message::LibraryRefreshStarted(LibraryRefreshReason::Startup),
        );
        let startup_library_task = self
            .environment
            .ctx
            .begin_library_refresh(LibraryRefreshReason::Startup)
            .map_or_else(Task::none, |task| {
                task.map(|result| {
                    RootMessage::LibraryRefreshed(LibraryRefreshReason::Startup, result)
                })
            });
        // Steam may already be connected before the event subscription starts.
        let steam_bootstrap_task = if self.environment.ctx.steam_connected() {
            Task::done(RootMessage::SteamSession(
                steam_session::Message::ConnectionEvent(steam_session::ConnectionEvent::Connected),
            ))
        } else {
            Task::none()
        };
        let update_check_task = self
            .environment
            .ctx
            .run_blocking("shell-update-check", |_app| {
                shell::fetch_latest_update(env!("CARGO_PKG_VERSION")).map_err(Arc::new)
            })
            .map(|result| RootMessage::UpdateCheckCompleted(result.into()));
        // Warm the font database off-thread before the analyzer needs it in update().
        let label_context_warmup_task = self
            .environment
            .ctx
            .run_blocking("label-context-warmup", |_app| {
                crate::media::size_analyzer_render::with_shared_label_context(|_context| ());
            })
            .discard();

        self.sync_game_prerequisite();
        let steam_installed_task = self.steam_installed_probe_task();

        Task::batch([
            system::theme().map(RootMessage::SystemThemeObserved),
            steam_installed_task,
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
        let update = UpdateContext::for_message(&message);
        self.update_at(message, update)
    }

    fn update_at(&mut self, message: RootMessage, update: UpdateContext) -> Task<RootMessage> {
        self.update_context = update;
        match message {
            RootMessage::Shell(message) => self.apply_shell_message(message, update),
            RootMessage::MyWorkshop(message) => self.apply_my_workshop_message(message),
            RootMessage::InstalledAddons(message) => self.apply_installed_addons_message(message),
            RootMessage::Downloader(message) => self.apply_downloader_message(message, update),
            RootMessage::SizeAnalyzer(message) => self.apply_size_analyzer_message(message),
            RootMessage::Search(message) => self.apply_search_message(message, update),
            RootMessage::DestinationSelect(message) => {
                self.apply_destination_select_message(message)
            }
            RootMessage::FilePreview(message) => self.apply_file_preview_message(message),
            RootMessage::PreparePublish(message) => {
                self.prepare_publish_message_task(message, update)
            }
            RootMessage::PreviewGma(message) => self.apply_preview_gma_message(message, update),
            RootMessage::DescriptionEditor(message) => {
                self.apply_description_editor_message(message, update)
            }
            RootMessage::Settings(message) => {
                let completed = matches!(&message, settings::Message::SaveCompleted(generation, _)
                    if self.settings_save_in_flight.as_ref().is_some_and(|(active, _)| active == generation));
                let reset_completed = matches!(&message, settings::Message::ResetCompleted(_, _));
                let task = self.apply_settings_message(message);
                if reset_completed {
                    self.settings_reset_in_flight = false;
                    Task::batch([task, self.start_next_settings_save()])
                } else if completed {
                    self.settings_save_in_flight = None;
                    Task::batch([task, self.start_next_settings_save()])
                } else {
                    task
                }
            }
            RootMessage::ContextMenu(message) => self.apply_context_menu_message(message, update),
            RootMessage::SteamSession(message) => self.apply_steam_session_message(message),
            RootMessage::Prerequisites(message) => self.apply_prerequisites_message(message),
            RootMessage::SteamClientInstalledProbed(installed) => {
                self.state
                    .features
                    .prerequisites
                    .set_steam_installed(installed);
                Task::none()
            }
            RootMessage::ModalStack(message) => self.apply_modal_stack_message(&message, update),
            RootMessage::SystemThemeObserved(mode) => {
                self.state.apply_system_theme(mode);
                Task::none()
            }
            RootMessage::DragRegionPressed => self.window_drag_task(),
            RootMessage::DragRegionDoubleClicked => self.window_toggle_maximize_task(),
            RootMessage::TaskEvent(event) => {
                let overlay = self.apply_tasks_overlay_message(
                    tasks_overlay::Message::TaskEventsReceived(vec![event.clone()]),
                    update,
                );
                let downloader = self.apply_downloader_message(
                    downloader::Message::TaskEventsReceived(vec![event]),
                    update,
                );
                Task::batch([overlay, downloader])
            }
            RootMessage::TasksOverlay(message) => self.apply_tasks_overlay_message(message, update),
            RootMessage::BackendEvent(event) => self.backend_event_task(event),
            RootMessage::AppDataSnapshotApplied(result) => {
                self.appdata_snapshot_in_flight = false;
                let applied = match result {
                    Ok(_) => {
                        // A save can publish newer settings before the worker result reaches the UI.
                        let (settings, paths) = self.environment.ctx.settings_and_paths_snapshot();
                        let snapshot = settings::SettingsSnapshot::new(
                            settings,
                            paths,
                            self.state.system_scheme,
                        );
                        self.apply_settings_snapshot_runtime(&snapshot)
                    }
                    Err(error) => {
                        log::warn!("failed to apply backend settings snapshot: {error}");
                        Task::none()
                    }
                };
                let next = self
                    .pending_appdata_snapshot
                    .take()
                    .map_or_else(Task::none, |snapshot| {
                        self.start_appdata_snapshot_task(snapshot)
                    });
                Task::batch([applied, next])
            }
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
            RootMessage::UpdateCheckCompleted(outcome) => match outcome {
                UpdateCheckOutcome::ReleaseAvailable(release) => {
                    self.apply_shell_message(shell::Message::UpdateReleaseFound(release), update)
                }
                UpdateCheckOutcome::UpToDate => Task::none(),
                UpdateCheckOutcome::CheckFailed(error) => {
                    log::debug!("update check failed: {error}");
                    Task::none()
                }
                UpdateCheckOutcome::WorkerFailed(error) => {
                    log::warn!("update check worker failed: {error}");
                    Task::none()
                }
            },
            RootMessage::ThumbnailDemand(thumbnail_demand::Message::Delivered(delivery)) => {
                log_thumbnail_delivery(&delivery);
                if let thumbnail_demand::DeliveryResult::Ready(ready) = &delivery.result
                    && let (Some(url), Some(hash)) = (delivery.key.source_url(), ready.thumbhash())
                {
                    self.environment.ctx.record_thumbhash(url, hash);
                }
                self.state.apply_thumbnail_delivery(&delivery);
                Task::none()
            }
            RootMessage::WarmLibraryResolved(preview_urls) => {
                self.warm_library_demands_task(preview_urls)
            }
            RootMessage::ThumbnailDemand(message) => self
                .environment
                .thumbnails
                .update(&self.environment.ctx, message)
                .map(RootMessage::ThumbnailDemand),
            RootMessage::FirstFramePresented(id) => {
                // iced_winit broadcasts the raw interaction while handling
                // the redraw, then finishes compositor presentation before
                // processing the subscription's resulting app message.
                self.window_id = Some(id);
                self.activate_startup_after_first_frame()
            }
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
                if matches!(shortcut, GlobalShortcut::ToggleSettings)
                    && self.settings_shortcut_should_close()
                {
                    return Task::done(RootMessage::Settings(settings::Message::CloseRequested));
                }
                if !self.global_shortcuts_enabled() {
                    return Task::none();
                }

                match shortcut {
                    GlobalShortcut::ToggleSearch => self.toggle_search_palette_task(update),
                    GlobalShortcut::ToggleFileSearch => {
                        self.toggle_file_search_palette_task(update)
                    }
                    GlobalShortcut::ToggleSettings => Task::batch([
                        self.dismiss_account_menu_task(update),
                        self.settings_open_task(),
                    ]),
                    GlobalShortcut::NavigateRoute(route) => {
                        Task::done(RootMessage::Shell(shell::Message::Navigate(route)))
                    }
                }
            }
            RootMessage::Platform(message) => self.platform_task(message),
            RootMessage::FileDropped(path) => self.handle_file_drop(path),
            RootMessage::Noop => Task::none(),
            RootMessage::AnimationTick(now) => {
                self.state.features.context_menu.tick(now);
                self.state.features.prerequisites.tick(now);
                self.state.features.shell.tick_motion(now);
                let _changed = self.state.features.tasks_overlay.tick(now);
                let palette_close_settled = self.state.features.search.tick_motion(now);
                self.state
                    .features
                    .prepare_publish
                    .tick_browser_select_hover(now);
                self.state.features.downloader.tick_progress(now);
                let mut finish_tasks = Vec::new();
                if self.state.tick_density_motion(now) {
                    finish_tasks.push(self.refresh_ui_metrics_task());
                }
                if palette_close_settled {
                    // The fading rows kept their thumbnails demanded; release
                    // them after the palette has fully reset.
                    finish_tasks.push(self.search_thumbnail_demands());
                }
                for modal in self.state.features.modal_stack.tick(now) {
                    finish_tasks.push(self.finish_modal_close_task(modal));
                }
                Task::batch(finish_tasks)
            }
        }
    }

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

    feature_dispatch! {
        apply_installed_addons_message => installed_addons.installed_addons, run_installed_addons_effect;
        apply_size_analyzer_message => size_analyzer.size_analyzer, run_size_analyzer_effect;
        apply_destination_select_message => destination_select.destination_select, run_destination_select_effect;
        apply_settings_message => settings.settings, run_settings_effect;
    }

    fn apply_downloader_message(
        &mut self,
        message: downloader::Message,
        update: UpdateContext,
    ) -> Task<RootMessage> {
        let effects =
            downloader::update_at(&mut self.state.features.downloader, message, update.now);
        self.batch_effects(effects, Self::run_downloader_effect)
    }

    fn apply_search_message(
        &mut self,
        message: search::Message,
        update: UpdateContext,
    ) -> Task<RootMessage> {
        let effects = search::update_at(&mut self.state.features.search, message, update.now);
        self.batch_effects(effects, Self::run_search_effect)
    }

    fn apply_preview_gma_message(
        &mut self,
        message: preview_gma::Message,
        update: UpdateContext,
    ) -> Task<RootMessage> {
        self.state.features.preview_gma.set_viewer(
            self.state
                .features
                .steam_session
                .identity()
                .map(steam_session::SteamIdentity::steamid),
            gmpublished_backend::WorkshopLanguage::from_locale(self.state.i18n.locale_id()),
        );
        let effects =
            preview_gma::update_at(&mut self.state.features.preview_gma, message, update.now);
        self.batch_effects(effects, Self::run_preview_gma_effect)
    }

    fn apply_tasks_overlay_message(
        &mut self,
        message: tasks_overlay::Message,
        update: UpdateContext,
    ) -> Task<RootMessage> {
        let effects =
            tasks_overlay::update_at(&mut self.state.features.tasks_overlay, message, update.now);
        self.batch_effects(effects, |app, effect| match effect {
            tasks_overlay::Effect::CancelRequested(task_id) => {
                let _cancelled = app.environment.ctx.cancel_task(task_id);
                Task::none()
            }
        })
    }

    fn apply_shell_message(
        &mut self,
        message: shell::Message,
        update: UpdateContext,
    ) -> Task<RootMessage> {
        let effects = shell::update_at(
            &mut self.state.features.shell,
            message,
            &self.state.tokens,
            self.state.chrome_strategy,
            update.now,
        );
        self.batch_effects(effects, Self::run_shell_effect)
    }

    #[cfg(target_os = "macos")]
    fn install_macos_menu(&self) {
        #[cfg(not(test))]
        platform_menu::install(&self.state.i18n);
    }

    #[cfg(target_os = "macos")]
    fn platform_task(&self, message: crate::platform::Message) -> Task<RootMessage> {
        use crate::platform::Message;
        match message {
            Message::MenuCommand(command) => self.menu_command_task(command),
            Message::MenuOpenGmaCompleted(path) => path.map_or_else(Task::none, |path| {
                Task::done(RootMessage::FileDropped(path))
            }),
            Message::SystemAppearanceChanged => self
                .window_id
                .map_or_else(Task::none, |id| self.traffic_light_position_task(id)),
            Message::GmaDocumentsOpened(paths) => self.gma_documents_opened_task(paths),
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[expect(
        clippy::unused_self,
        clippy::needless_pass_by_value,
        reason = "matches the macOS handler, which dispatches on self and consumes the message"
    )]
    fn platform_task(&self, message: crate::platform::Message) -> Task<RootMessage> {
        match message {}
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

    fn apply_my_workshop_message(&mut self, message: my_workshop::Message) -> Task<RootMessage> {
        #[cfg(feature = "debug")]
        let message = self.filter_my_workshop_message(message);
        let effects = my_workshop::update(&mut self.state.features.my_workshop, message);
        self.batch_effects(effects, Self::run_my_workshop_effect)
    }

    #[cfg(feature = "debug")]
    fn filter_my_workshop_message(&self, message: my_workshop::Message) -> my_workshop::Message {
        match message {
            my_workshop::Message::PageCompleted(generation, page, Ok(mut result)) => {
                result.retain_visible(self.state.hidden_addons.workshop_ids());
                my_workshop::Message::PageCompleted(generation, page, Ok(result))
            }
            my_workshop::Message::StatsRefreshCompleted(generation, Ok(mut counts)) => {
                counts.retain(|workshop_id, _| {
                    !self.state.hidden_addons.contains_workshop_id(*workshop_id)
                });
                my_workshop::Message::StatsRefreshCompleted(generation, Ok(counts))
            }
            message => message,
        }
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
                    if !self.environment.ctx.cancel_task(task_id) {
                        log::debug!(
                            "downloader cancellation for task {task_id:?} had no effect (already terminal or not yet correlated)"
                        );
                    }
                }
                Task::none()
            }
            downloader::Effect::DownloadQueueCancellationRequested => {
                self.environment.ctx.cancel_all_workshop_downloads();
                Task::none()
            }
            downloader::Effect::PathsOpenRequested(paths) => self.downloader_open_paths_task(paths),
            downloader::Effect::PreviewRequested(target) => self.apply_preview_gma_message(
                preview_gma::Message::OpenRequested(preview_gma::OpenTarget::new(
                    target.path,
                    target.title,
                    target.workshop_id,
                )),
                self.update_context,
            ),
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
            downloader::Effect::ActiveJobCountChanged(count) => self.apply_shell_message(
                shell::Message::DownloaderJobCountChanged(count),
                self.update_context,
            ),
            downloader::Effect::PrerequisiteActivated(action) => {
                self.prerequisite_action_task(action)
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
            my_workshop::Effect::GridScrollAnchored(delta) => iced::widget::operation::scroll_by(
                addon_grid::scrollable_id(my_workshop::GRID_KEY),
                iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: delta },
            ),
            my_workshop::Effect::AddonDragPressed {
                card_id,
                workshop_id,
            } => {
                let thumbnail = self
                    .state
                    .features
                    .my_workshop
                    .drag_thumbnail_for_card(&card_id);
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
            // Use relative scrolling. The mirrored offset is rounded to u32, so absolute
            // anchors would accumulate drift and discard concurrent user scrolls.
            installed_addons::Effect::GridScrollAnchored(delta) => {
                iced::widget::operation::scroll_by(
                    addon_grid::scrollable_id(installed_addons::GRID_KEY),
                    iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: delta },
                )
            }
            installed_addons::Effect::AddonDragPressed {
                card_id,
                workshop_id,
            } => {
                let thumbnail = self
                    .state
                    .features
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
                    .and_then(|id| self.state.features.size_analyzer.tile_for(id))
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
                if !self.environment.ctx.cancel_task(task_id) {
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
                    self.state
                        .features
                        .destination_select
                        .initial_browse_directory(),
                ),
            destination_select::Effect::CustomPathValidationRequested(request) => self
                .environment
                .ctx
                .run_blocking("destination-validate-path", move |_app| {
                    let valid = destination_select::probe_custom_path(&request.path);
                    (request, valid)
                })
                .map(|result| match result {
                    Ok((request, valid)) => RootMessage::DestinationSelect(
                        destination_select::Message::CustomPathValidated {
                            generation: request.generation,
                            path: request.path,
                            valid,
                        },
                    ),
                    Err(error) => {
                        log::warn!("destination path validation failed: {error}");
                        RootMessage::Noop
                    }
                }),
            destination_select::Effect::CreateFolderChanged(enabled) => {
                self.destination_select_create_folder_task(enabled)
            }
            destination_select::Effect::DestinationPersistRequested(request) => {
                self.destination_select_save_task(request)
            }
            destination_select::Effect::DestinationPersisted => Task::batch([
                if self.state.features.modal_stack.overlay_active() {
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
            preview_gma::Effect::EditMetadataRequested {
                workshop_id,
                language,
            } => self.apply_description_editor_message(
                description_editor::Message::OpenLocalizedRequested {
                    workshop_id,
                    title: None,
                    language,
                },
                self.update_context,
            ),
            preview_gma::Effect::LocalizedTextRequested {
                request_id,
                workshop_id,
                language,
            } => self
                .environment
                .ctx
                .run_blocking("preview-localized-text", move |app| {
                    app.localized_text(workshop_id, language)
                })
                .map(move |result| {
                    RootMessage::PreviewGma(preview_gma::Message::LocalizedTextFetched(
                        request_id,
                        flatten_blocking_ui_result(result),
                    ))
                }),
            preview_gma::Effect::FilePreview(message) => self.apply_file_preview_message(message),
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
                    .features
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
            settings::Effect::OpenUrlRequested(url) => self.open_url_task(url.to_owned()),
            settings::Effect::DebugInfoRequested(generation) => {
                self.settings_debug_info_task(generation)
            }
            settings::Effect::CopyDebugInfoRequested(generation, report) => self
                .copy_text_task(report)
                .chain(Task::done(RootMessage::Settings(
                    settings::Message::DebugInfoCopied(generation),
                ))),
            settings::Effect::PathBrowseRequested(kind) => self.settings_folder_picker_task(kind),
            settings::Effect::PathValidationRequested(request) => {
                self.settings_path_validation_task(request)
            }
            settings::Effect::MutationApplied(generation, mutation) => {
                let runtime_task = self.apply_settings_mutation_runtime(&mutation);
                let save_task = self.enqueue_settings_save(generation, mutation);
                Task::batch([runtime_task, save_task])
            }
            settings::Effect::FocusMoved(reverse) => {
                use iced::advanced::widget::operation::{focusable, scope};
                let id = iced::widget::Id::new("settings-modal");
                if reverse {
                    iced::advanced::widget::operate(scope(id, focusable::focus_previous()))
                } else {
                    iced::advanced::widget::operate(scope(id, focusable::focus_next()))
                }
            }
            settings::Effect::PreviewApplied(mutation) => {
                self.apply_settings_mutation_runtime(&mutation)
            }
            settings::Effect::SnapshotApplied(snapshot) => {
                self.apply_settings_snapshot_runtime(&snapshot)
            }
            settings::Effect::ResetRunRequested(action) => {
                self.pending_settings_reset = Some(action);
                self.start_next_settings_save()
            }
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
        let route_body: Element<'_, RootMessage> = container(crate::widgets::settle::settle(
            self.active_route_view(),
            self.state.features.shell.route() as u64,
            &tokens,
        ))
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
                &self.state.features.shell,
                ctx,
                self.state.chrome_strategy,
                self.state.viewport_size.width,
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
        // Exclude the sidebar or this layer masks the Downloader drop-target cursor.
        if self.state.addon_drag.is_dragging() {
            layers = layers.push(addon_drag_cursor_overlay(&self.state, &tokens, now));
        }
        if let Some(dropdown) = search::dropdown_overlay(
            &self.state.features.search,
            ctx,
            self.state.viewport_size,
            now,
        ) {
            layers = layers.push(dropdown.map(RootMessage::Search));
        }

        if let Some(account_menu) = shell::account_menu_overlay(
            &self.state.features.shell,
            ctx,
            self.state.viewport_size,
            now,
        ) {
            layers = layers.push(account_menu.map(RootMessage::Shell));
        }

        let chrome_clearance = if self.state.chrome_strategy.mac_native_inset() {
            tokens.dims.sidebar_band_height
        } else {
            0.0
        };

        if let Some(active) = self.state.features.modal_stack.active() {
            let modal_scale = self.state.features.modal_stack.scale(now);
            let modal_interactive = self.state.features.modal_stack.interactive();
            layers = layers.push(
                modal_stack::scrim(&self.state.features.modal_stack, &tokens, now)
                    .map(RootMessage::ModalStack),
            );

            let hosts_preview = matches!(
                active,
                modal_stack::ActiveModal::PreparePublish | modal_stack::ActiveModal::PreviewGma
            );
            if hosts_preview && file_preview::embedded_expanded(&self.state.features.file_preview) {
                layers = layers.push(
                    modal_stack::expanded_scrim(&self.state.features.modal_stack, &tokens, now)
                        .map(RootMessage::ModalStack),
                );
            }

            let content = match active {
                modal_stack::ActiveModal::PreparePublish => Some(
                    prepare_publish::view(
                        &self.state.features.prepare_publish,
                        &self.state.features.file_preview,
                        ctx,
                        self.state.viewport_size,
                        chrome_clearance,
                        now,
                    )
                    .map(RootMessage::PreparePublish),
                ),
                modal_stack::ActiveModal::PreviewGma => Some(
                    preview_gma::view(
                        &self.state.features.preview_gma,
                        &self.state.features.file_preview,
                        ctx,
                        self.state.viewport_size,
                        chrome_clearance,
                    )
                    .map(RootMessage::PreviewGma),
                ),
                modal_stack::ActiveModal::Settings => Some(
                    settings::view(&self.state.features.settings, ctx, self.state.viewport_size)
                        .map(RootMessage::Settings),
                ),
                modal_stack::ActiveModal::DestinationSelect
                | modal_stack::ActiveModal::DescriptionEditor => None,
            };
            if let Some(content) = content {
                layers = layers.push(modal_stack::frame(content, modal_scale, modal_interactive));
            }
        }

        if let Some(overlay_modal) = self.state.features.modal_stack.overlay_modal() {
            let overlay_scale = self.state.features.modal_stack.overlay_scale(now);
            let overlay_interactive = self.state.features.modal_stack.overlay_interactive();
            layers = layers.push(
                modal_stack::overlay_scrim(&self.state.features.modal_stack, &tokens, now)
                    .map(RootMessage::ModalStack),
            );

            let content = match overlay_modal {
                modal_stack::ActiveModal::DestinationSelect => Some(
                    destination_select::view(
                        &self.state.features.destination_select,
                        ctx,
                        self.state.viewport_size,
                    )
                    .map(RootMessage::DestinationSelect),
                ),
                modal_stack::ActiveModal::DescriptionEditor => Some(
                    description_editor::view(
                        &self.state.features.description_editor,
                        ctx,
                        self.state.viewport_size,
                        chrome_clearance,
                    )
                    .map(RootMessage::DescriptionEditor),
                ),
                modal_stack::ActiveModal::PreparePublish
                | modal_stack::ActiveModal::PreviewGma
                | modal_stack::ActiveModal::Settings => None,
            };
            if let Some(content) = content {
                layers = layers.push(modal_stack::frame(
                    content,
                    overlay_scale,
                    overlay_interactive,
                ));
            }
        }

        if self.state.features.context_menu.visible() {
            layers = layers.push(
                context_menu::view(
                    &self.state.features.context_menu,
                    ctx,
                    self.state.viewport_size,
                    now,
                )
                .map(RootMessage::ContextMenu),
            );
        }

        if self.state.addon_drag.is_dragging() {
            layers = layers.push(addon_drag_ghost(&self.state.addon_drag, ctx));
        }

        if let Some(overlay) = tasks_overlay::view(
            &self.state.features.tasks_overlay,
            ctx,
            self.state.viewport_size,
            now,
        ) {
            layers = layers.push(overlay.map(RootMessage::TasksOverlay));
        }

        // Capture shortcuts before a focused text input can consume them.
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
        let route = self.state.features.shell.route();
        let blocker = prerequisites::blocker_for(
            route,
            self.state.features.steam_session.status(),
            &self.state.features.prerequisites,
        );
        let panel = |blocker: &prerequisites::Blocker| {
            prerequisites::view(
                ctx,
                route,
                blocker,
                self.state.features.prerequisites.spinner_elapsed(),
                |action| RootMessage::Prerequisites(prerequisites::Message::Activated(action)),
            )
        };

        match (route, blocker.as_ref()) {
            (shell::Route::Downloader, blocker) => downloader::view(
                &self.state.features.downloader,
                ctx,
                blocker
                    .filter(|_| self.state.features.downloader.queue_is_empty())
                    .map(|blocker| {
                        prerequisites::view(
                            ctx,
                            route,
                            blocker,
                            self.state.features.prerequisites.spinner_elapsed(),
                            downloader::Message::Prerequisite,
                        )
                    }),
            )
            .map(RootMessage::Downloader),
            (_, Some(blocker)) => panel(blocker),
            (shell::Route::MyWorkshop, None) => {
                my_workshop::view(&self.state.features.my_workshop, ctx)
                    .map(RootMessage::MyWorkshop)
            }
            (shell::Route::InstalledAddons, None) => {
                installed_addons::view(&self.state.features.installed_addons, ctx)
                    .map(RootMessage::InstalledAddons)
            }
            (shell::Route::SizeAnalyzer, None) => {
                size_analyzer::view(&self.state.features.size_analyzer, ctx)
                    .map(RootMessage::SizeAnalyzer)
            }
        }
    }

    fn request_library_refresh(&mut self, reason: LibraryRefreshReason) -> Task<RootMessage> {
        let started = self.apply_installed_addons_message(
            installed_addons::Message::LibraryRefreshStarted(reason),
        );
        let refresh = self
            .environment
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
                    .environment
                    .ctx
                    .abort_library_refresh()
                    .map_or_else(Task::none, |reason| self.request_library_refresh(reason));
                return Task::batch([installed, analyzer, rerun]);
            }
        };

        let visible_snapshot = refresh
            .snapshot
            .as_ref()
            .map(|snapshot| self.state.hidden_addons.visible(snapshot));

        sync_search_installed_addons(&self.environment.ctx, visible_snapshot.as_ref());

        let warm_library = visible_snapshot
            .as_ref()
            .map_or_else(Task::none, |snapshot| self.warm_library_kick_task(snapshot));

        let rows = visible_snapshot.as_ref().map_or_else(
            || {
                Err(UiError::new(
                    gmpublished_backend::error_keys::GMOD_PATH_MISSING,
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
        self.open_search_mode_palette_task(SearchMode::Addons, self.update_context)
    }

    fn open_search_mode_palette_task(
        &mut self,
        mode: SearchMode,
        update: UpdateContext,
    ) -> Task<RootMessage> {
        Task::batch([
            self.dismiss_account_menu_task(update),
            self.apply_search_message(search::Message::ModeFocusRequested(mode), update),
        ])
    }

    fn toggle_search_palette_task(&mut self, update: UpdateContext) -> Task<RootMessage> {
        if self.state.features.search.palette_open()
            && self.state.features.search.mode() == SearchMode::Addons
        {
            self.apply_search_message(search::Message::DismissRequested, update)
        } else {
            self.open_search_mode_palette_task(SearchMode::Addons, update)
        }
    }

    fn toggle_file_search_palette_task(&mut self, update: UpdateContext) -> Task<RootMessage> {
        if self.state.features.search.palette_open()
            && self.state.features.search.mode() == SearchMode::Files
        {
            self.apply_search_message(search::Message::DismissRequested, update)
        } else {
            self.open_search_mode_palette_task(SearchMode::Files, update)
        }
    }

    fn dismiss_account_menu_task(&mut self, update: UpdateContext) -> Task<RootMessage> {
        self.apply_shell_message(shell::Message::AccountMenuDismissed, update)
    }

    fn dismiss_search_palette_task(&mut self) -> Task<RootMessage> {
        if !self.state.features.search.palette_open() {
            return Task::none();
        }

        self.apply_search_message(search::Message::DismissRequested, self.update_context)
    }

    fn global_shortcuts_enabled(&self) -> bool {
        self.state.features.modal_stack.active().is_none()
            && !self.state.features.modal_stack.overlay_active()
            && !self.state.features.context_menu.visible()
    }

    fn settings_shortcut_should_close(&self) -> bool {
        self.state.features.modal_stack.active() == Some(modal_stack::ActiveModal::Settings)
            && !self.state.features.modal_stack.overlay_active()
            && !self.state.features.context_menu.visible()
    }

    pub(crate) fn subscription(&self) -> Subscription<RootMessage> {
        let mut streams = vec![
            self.environment
                .ctx
                .task_events()
                .map(RootMessage::TaskEvent),
        ];
        streams.push(
            self.environment
                .ctx
                .backend_events()
                .map(RootMessage::BackendEvent),
        );
        streams.push(shell::subscription(&self.state.features.shell).map(RootMessage::Shell));
        if self.state.follows_system_theme() {
            streams.push(system::theme_changes().map(RootMessage::SystemThemeObserved));
        }
        streams.push(
            theme::motion::redraw_subscription(self.state.needs_motion_ticks())
                .map(RootMessage::AnimationTick),
        );
        streams.push(
            my_workshop::subscription(&self.state.features.my_workshop)
                .map(RootMessage::MyWorkshop),
        );
        if self.startup_phase.started() {
            streams.push(
                library_watch::subscription(
                    self.state.features.installed_addons.watch_gmod_dir(),
                    self.state.features.installed_addons.watch_arm_epoch(),
                )
                .map(RootMessage::LibraryWatch),
            );
        }
        streams.push(
            installed_addons::subscription(&self.state.features.installed_addons)
                .map(RootMessage::InstalledAddons),
        );
        streams.push(
            prerequisites::subscription(
                self.state.features.shell.route(),
                self.state.features.steam_session.status(),
            )
            .map(RootMessage::Prerequisites),
        );
        streams.push(
            prepare_publish::subscription(&self.state.features.prepare_publish)
                .map(RootMessage::PreparePublish),
        );
        streams.push(
            description_editor::subscription(&self.state.features.description_editor)
                .map(RootMessage::DescriptionEditor),
        );
        streams.push(
            preview_gma::subscription(&self.state.features.preview_gma)
                .map(RootMessage::PreviewGma),
        );
        streams.push(
            file_preview::subscription(&self.state.features.file_preview)
                .map(RootMessage::FilePreview),
        );
        streams.push(search::subscription(&self.state.features.search).map(RootMessage::Search));
        streams
            .push(settings::subscription(&self.state.features.settings).map(RootMessage::Settings));
        streams.push(
            context_menu::subscription(&self.state.features.context_menu)
                .map(RootMessage::ContextMenu),
        );
        // The context menu owns Escape while visible. Global shortcuts are
        // handled by `shortcut_capture` before the widget tree sees them.
        if !self.state.features.context_menu.visible() {
            streams.push(
                modal_stack::subscription(&self.state.features.modal_stack)
                    .map(RootMessage::ModalStack),
            );
        }
        streams.push(window::events().map(|(id, event)| RootMessage::WindowEvent(id, event)));
        if !self.startup_phase.started() {
            // `window::events` filters redraws. Listen for one frame to avoid an idle frame loop.
            streams.push(event::listen_raw(first_frame_presented_event));
        }
        if self.state.addon_drag.is_active() {
            streams.push(event::listen_with(addon_drag_event));
        }
        streams.push(event::listen_with(file_drop_event));
        streams.extend(
            crate::platform::subscriptions()
                .into_iter()
                .map(|stream| stream.map(RootMessage::Platform)),
        );
        Subscription::batch(streams)
    }

    pub(crate) fn theme(&self) -> Option<Theme> {
        None
    }

    pub(crate) fn title(&self) -> String {
        crate::APP_NAME.to_owned()
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
    let left = shell::sidebar_width(tokens, state.chrome_strategy, state.viewport_size.width);

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
