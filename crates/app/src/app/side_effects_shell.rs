#[cfg(target_os = "macos")]
use super::shell;
use super::{
    App, NativeOpenTarget, PathBuf, Point, PublishedFileId, RootMessage, Task, UpdateContext,
    context_menu, destination_select, downloader, file_preview, flatten_blocking_ui_result,
    installed_addons, modal_stack, my_workshop, open_modal_message, prepare_publish, preview_gma,
    schedule_native_open_target, settings, size_analyzer, window, workshop_url,
};
#[cfg(feature = "debug")]
use crate::bridge::tasks::TransactionStatus;
use crate::bridge::ui_error::ResultExt as _;
use crate::features::context_menu::{ContextMenuTarget, LocalMenuTarget};

impl App {
    pub(super) fn finish_modal_close_task(
        &mut self,
        modal: modal_stack::ActiveModal,
    ) -> Task<RootMessage> {
        match modal {
            modal_stack::ActiveModal::DestinationSelect => Task::done(
                RootMessage::DestinationSelect(destination_select::Message::CloseFinished),
            ),
            modal_stack::ActiveModal::PreviewGma => self.apply_preview_gma_message(
                preview_gma::Message::CloseFinished,
                self.update_context,
            ),
            modal_stack::ActiveModal::PreparePublish => self.apply_prepare_publish_message(
                prepare_publish::Message::CloseRequested,
                self.update_context,
            ),
            modal_stack::ActiveModal::Settings => {
                self.apply_settings_message(settings::Message::CloseFinished)
            }
        }
    }

    pub(super) fn apply_modal_stack_message(
        &mut self,
        message: &modal_stack::Message,
        update: UpdateContext,
    ) -> Task<RootMessage> {
        if matches!(message, modal_stack::Message::CloseRequested) {
            self.modal_stack_close_requested_task(update)
        } else {
            self.modal_stack_task(message, update)
        }
    }

    fn modal_stack_close_requested_task(&mut self, update: UpdateContext) -> Task<RootMessage> {
        if self.state.features.modal_stack.overlay_active() {
            return self.modal_stack_task(&modal_stack::Message::CloseRequested, update);
        }

        // Settings only shields itself while it is the TOPMOST layer; with
        // the Destination Select overlay above it, close requests target the
        // overlay.
        if self.state.features.modal_stack.active() == Some(modal_stack::ActiveModal::Settings)
            && self.state.features.settings.blocks_scrim_close()
        {
            return Task::none();
        }

        if matches!(
            self.state.features.modal_stack.active(),
            Some(modal_stack::ActiveModal::PreviewGma | modal_stack::ActiveModal::PreparePublish)
        ) && self.state.features.file_preview.is_open()
        {
            let message = if self.state.features.file_preview.expanded() {
                file_preview::Message::ExpandToggled
            } else {
                file_preview::Message::BackRequested
            };
            return self.apply_file_preview_message(message);
        }

        self.modal_stack_task(&modal_stack::Message::CloseRequested, update)
    }

    pub(super) fn modal_stack_task(
        &mut self,
        message: &modal_stack::Message,
        update: UpdateContext,
    ) -> Task<RootMessage> {
        let effects =
            modal_stack::update_at(&mut self.state.features.modal_stack, message, update.now);
        self.batch_effects(effects, |app, effect| match effect {
            modal_stack::Effect::Displaced(modal) => app.finish_modal_close_task(modal),
        })
    }

    pub(super) fn open_modal_stack_task(
        &mut self,
        modal: modal_stack::ActiveModal,
    ) -> Task<RootMessage> {
        Task::batch([
            self.dismiss_account_menu_task(self.update_context),
            self.dismiss_search_palette_task(),
            self.modal_stack_task(&open_modal_message(modal), self.update_context),
        ])
    }

    pub(super) fn close_modal_stack_task(&mut self) -> Task<RootMessage> {
        self.modal_stack_task(&modal_stack::Message::CloseRequested, self.update_context)
    }

    pub(super) fn window_event_task(
        &mut self,
        id: window::Id,
        event: &window::Event,
    ) -> Task<RootMessage> {
        self.window_id = Some(id);
        match event {
            window::Event::Opened { .. } => {
                let scale_factor_task = window::scale_factor(id).map(move |scale_factor| {
                    RootMessage::WindowScaleFactorObserved(id, scale_factor)
                });
                #[cfg(target_os = "macos")]
                self.install_macos_menu();
                Task::batch([
                    scale_factor_task,
                    self.traffic_light_position_task(id),
                    self.traffic_light_keepalive_task(id),
                ])
            }
            window::Event::Rescaled(scale_factor) => {
                self.apply_window_scale_factor(id, *scale_factor)
            }
            window::Event::Focused => {
                self.state.set_window_focused(true);
                // Idempotent insurance: AppKit can rebuild the titlebar while
                // the window is in the background (e.g. appearance change),
                // resetting the custom traffic-light frames.
                self.traffic_light_position_task(id)
            }
            window::Event::Unfocused => {
                self.state.set_window_focused(false);
                Task::none()
            }
            // Resizes are handled natively: the keepalive observer re-asserts
            // the traffic-light treatment synchronously inside each resize
            // layout pass, which an async round-trip through this message
            // loop is always at least a frame too late for.
            window::Event::Closed
            | window::Event::Resized(_)
            | window::Event::Moved(_)
            | window::Event::RedrawRequested(_)
            | window::Event::CloseRequested
            | window::Event::FileHovered(_)
            | window::Event::FileDropped(_)
            | window::Event::FilesHoveredLeft => Task::none(),
        }
    }

    #[cfg(target_os = "macos")]
    pub(super) fn traffic_light_position_task(&self, id: window::Id) -> Task<RootMessage> {
        if !self.state.chrome_strategy.mac_native_inset() {
            return Task::none();
        }

        let tokens = self.state.tokens;
        window::run(
            id,
            crate::platform_chrome::position_traffic_lights(
                shell::traffic_light_origin_x(&tokens) as f64,
                shell::traffic_light_center_y(&tokens) as f64,
            ),
        )
        .discard()
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn traffic_light_position_task(&self, _id: window::Id) -> Task<RootMessage> {
        Task::none()
    }

    /// Installs the native observer that keeps the traffic-light treatment
    /// applied through live resizes. Installed unconditionally (once, at
    /// window open) because the titlebar mode can be switched at runtime;
    /// the observer itself no-ops while the system titlebar is active.
    #[cfg(target_os = "macos")]
    fn traffic_light_keepalive_task(&self, id: window::Id) -> Task<RootMessage> {
        let tokens = self.state.tokens;
        window::run(
            id,
            crate::platform_chrome::install_resize_keepalive(
                shell::traffic_light_origin_x(&tokens) as f64,
                shell::traffic_light_center_y(&tokens) as f64,
            ),
        )
        .discard()
    }

    #[cfg(not(target_os = "macos"))]
    fn traffic_light_keepalive_task(&self, _id: window::Id) -> Task<RootMessage> {
        Task::none()
    }

    pub(super) fn apply_window_scale_factor(
        &mut self,
        id: window::Id,
        scale_factor: f32,
    ) -> Task<RootMessage> {
        if self.window_id.is_some_and(|active| active != id) {
            return Task::none();
        }

        self.window_id = Some(id);
        let analyzer_task = if self.state.set_scale_factor(scale_factor) {
            self.apply_size_analyzer_message(size_analyzer::Message::ScaleFactorChanged)
        } else {
            Task::none()
        };
        let thumbnail_task = if self.environment.thumbnails.set_scale_factor(scale_factor) {
            self.thumbnail_scale_changed_task()
        } else {
            Task::none()
        };
        Task::batch([analyzer_task, thumbnail_task])
    }

    pub(super) fn window_drag_task(&self) -> Task<RootMessage> {
        self.window_id.map_or_else(Task::none, window::drag)
    }

    pub(super) fn window_toggle_maximize_task(&self) -> Task<RootMessage> {
        self.window_id
            .map_or_else(Task::none, window::toggle_maximize)
    }

    pub(super) fn open_url_task(&self, url: String) -> Task<RootMessage> {
        self.native_open_target_task("native-open-url", NativeOpenTarget::url(url))
    }

    pub(super) fn native_open_target_task(
        &self,
        name: &'static str,
        target: NativeOpenTarget,
    ) -> Task<RootMessage> {
        let ctx = self.environment.ctx.clone();
        Task::future(async move {
            schedule_native_open_target(&ctx, name, target);
        })
        .discard()
    }

    pub(super) fn copy_text_task(&self, text: String) -> Task<RootMessage> {
        iced::clipboard::write(text)
    }

    /// Loads a fresh settings snapshot and opens the chooser with the
    /// caller's context (confirm label, extracted name, forced folder).
    pub(super) fn destination_select_open_task(
        &mut self,
        context: destination_select::OpenContext,
    ) -> Task<RootMessage> {
        let (settings, paths) = self.environment.ctx.settings_and_paths_snapshot();
        self.apply_destination_select_message(destination_select::Message::OpenRequested {
            snapshot: Box::new(destination_select::SettingsSnapshot::new(settings, paths)),
            context,
        })
    }

    /// Persists the create-folder checkbox immediately without closing the
    /// chooser or touching the pending destination choice.
    pub(super) fn destination_select_create_folder_task(&self, enabled: bool) -> Task<RootMessage> {
        self.environment
            .ctx
            .run_blocking("destination-create-folder-save", move |app| {
                app.config()
                    .update_settings_snapshot(|settings| {
                        settings.backend.create_folder_on_extract = enabled;
                    })
                    .map(|()| {
                        Box::new(destination_select::SettingsSnapshot::new(
                            app.config().settings_snapshot(),
                            app.config().paths(),
                        ))
                    })
                    .ui_err()
            })
            .map(|result| {
                RootMessage::DestinationSelect(destination_select::Message::CreateFolderSaved(
                    flatten_blocking_ui_result(result),
                ))
            })
    }

    pub(super) fn destination_select_folder_picker_task(
        &self,
        directory: PathBuf,
    ) -> Task<RootMessage> {
        let title = self
            .state
            .i18n
            .tr("native-dialog-select-extraction-destination");
        Task::future(async move {
            let selected = rfd::AsyncFileDialog::new()
                .set_title(title)
                .set_directory(directory)
                .set_can_create_directories(true)
                .pick_folder()
                .await
                .map(|folder| folder.path().to_path_buf());
            RootMessage::DestinationSelect(destination_select::Message::BrowseCompleted(selected))
        })
    }

    pub(super) fn destination_select_save_task(
        &self,
        request: destination_select::DestinationPersistRequest,
    ) -> Task<RootMessage> {
        self.environment
            .ctx
            .run_blocking("destination-select-save", move |app| {
                app.config()
                    .update_settings_snapshot(|settings| {
                        destination_select::apply_persist_request(settings, request);
                    })
                    .map(|()| {
                        Box::new(destination_select::SettingsSnapshot::new(
                            app.config().settings_snapshot(),
                            app.config().paths(),
                        ))
                    })
                    .ui_err()
            })
            .map(|result| {
                let result = flatten_blocking_ui_result(result);
                RootMessage::DestinationSelect(destination_select::Message::SaveCompleted(result))
            })
    }

    pub(super) fn open_my_workshop_context_menu(
        &mut self,
        menu: my_workshop::ContextMenuRequest,
    ) -> Task<RootMessage> {
        let my_workshop::ContextMenuRequest {
            position,
            workshop_id,
            workshop_url,
            preview_url,
            entries,
            ..
        } = menu;
        if entries.is_empty() {
            return Task::none();
        }

        let target = ContextMenuTarget::MyWorkshop {
            workshop_id,
            workshop_url,
            preview_url,
        };
        self.open_context_menu(position, entries, target)
    }

    pub(super) fn open_installed_addons_context_menu(
        &mut self,
        menu: installed_addons::ContextMenuRequest,
    ) -> Task<RootMessage> {
        let installed_addons::ContextMenuRequest {
            position,
            path,
            path_text,
            workshop_id,
            workshop_url,
            preview_url,
            entries,
            ..
        } = menu;
        if entries.is_empty() {
            return Task::none();
        }

        let target = ContextMenuTarget::Local(LocalMenuTarget {
            path,
            path_text,
            workshop_id,
            workshop_url,
            preview_url,
        });
        self.open_context_menu(position, entries, target)
    }

    pub(super) fn open_size_analyzer_context_menu(
        &mut self,
        menu: &size_analyzer::ContextMenuRequest,
    ) -> Task<RootMessage> {
        let entries = menu.entries().to_vec();
        if entries.is_empty() {
            return Task::none();
        }

        let target = menu.target();
        let workshop_id = target.workshop_id();
        let target = ContextMenuTarget::Local(LocalMenuTarget {
            path: target.path().to_path_buf(),
            path_text: target.path().display().to_string(),
            workshop_id,
            workshop_url: workshop_id.map(workshop_url::workshop_item_url),
            preview_url: target.preview_url().map(str::to_owned),
        });
        self.open_context_menu(menu.position, entries, target)
    }

    pub(super) fn open_context_menu(
        &self,
        position: Point,
        entries: Vec<context_menu::Entry>,
        target: ContextMenuTarget,
    ) -> Task<RootMessage> {
        Task::done(RootMessage::ContextMenu(
            context_menu::Message::OpenRequested(context_menu::OpenRequest::new(
                position, entries, target,
            )),
        ))
    }

    pub(super) fn apply_context_menu_message(
        &mut self,
        message: context_menu::Message,
        update: UpdateContext,
    ) -> Task<RootMessage> {
        let effects =
            context_menu::update_at(&mut self.state.features.context_menu, message, update.now);
        self.batch_effects(effects, Self::run_context_menu_effect)
    }

    fn run_context_menu_effect(&mut self, effect: context_menu::Effect) -> Task<RootMessage> {
        match effect {
            context_menu::Effect::ActionSelected(action) => self.route_context_menu_action(action),
            context_menu::Effect::Dismissed => Task::none(),
        }
    }

    pub(super) fn route_context_menu_action(
        &mut self,
        action: context_menu::ContextMenuAction,
    ) -> Task<RootMessage> {
        #[cfg(feature = "debug")]
        if let context_menu::ContextMenuAction::SimulateToast(kind) = action {
            return self.simulate_toast_task(kind);
        }

        let Some(target) = self.state.features.context_menu.take_target() else {
            return Task::none();
        };

        match (action, target) {
            #[cfg(feature = "debug")]
            (context_menu::ContextMenuAction::HideAddon, target) => {
                self.hide_context_menu_addon(target)
            }
            #[cfg(feature = "debug")]
            (
                context_menu::ContextMenuAction::AdjustSubscribers(delta),
                ContextMenuTarget::MyWorkshop { workshop_id, .. },
            ) => self.apply_my_workshop_message(my_workshop::Message::DebugSubscribersAdjusted {
                workshop_id,
                delta,
            }),
            #[cfg(feature = "debug")]
            (
                context_menu::ContextMenuAction::AdjustSubscribers(_),
                ContextMenuTarget::Local(_),
            ) => Task::none(),
            (action, ContextMenuTarget::Local(local)) => {
                self.route_local_context_menu_action(action, local)
            }
            (
                action,
                ContextMenuTarget::MyWorkshop {
                    workshop_id,
                    workshop_url,
                    preview_url,
                },
            ) => self.route_workshop_context_menu_action(
                action,
                Some(workshop_id),
                Some(workshop_url),
                preview_url,
            ),
        }
    }

    /// Drives a fake correlated transaction from a background thread so the
    /// simulated toast exercises the same event path as real work,
    /// cancellation included.
    #[cfg(feature = "debug")]
    fn simulate_toast_task(&self, kind: context_menu::SimulatedToast) -> Task<RootMessage> {
        use gmpublished_backend::TransactionPayload;
        use gmpublished_backend::error_keys as keys;

        use crate::bridge::tasks::TaskKind;
        use context_menu::SimulatedToast;

        const SIMULATED_TOTAL_BYTES: u64 = 25 * 1024 * 1024;

        if matches!(kind, SimulatedToast::Notice) {
            let task = self
                .environment
                .ctx
                .create_task(TaskKind::Notice, TransactionStatus::Notice);
            task.finished();
            return Task::none();
        }

        let ctx = self.environment.ctx.clone();
        let schedule = ctx
            .clone()
            .spawn_blocking_detached("simulate-toast", move |_| {
                let transaction = ctx.begin_transaction();
                let task = ctx.create_task(TaskKind::OverlayExtract, TransactionStatus::Extracting);
                task.total(SIMULATED_TOTAL_BYTES);
                ctx.correlate_backend_transaction(transaction.id(), task);

                let last_step = match kind {
                    SimulatedToast::Error => 40,
                    _ => 100,
                };
                for step in 0..=last_step {
                    if transaction.aborted() {
                        return;
                    }
                    transaction.progress(f64::from(step) / 100.0);
                    std::thread::sleep(std::time::Duration::from_millis(60));
                }
                let _ = match kind {
                    SimulatedToast::Error => transaction.error(keys::IO_ERROR),
                    _ => transaction.finished(TransactionPayload::None),
                };
            });
        if let Err(error) = schedule {
            log::warn!("failed to schedule simulated toast: {error}");
        }
        Task::none()
    }

    #[cfg(feature = "debug")]
    fn hide_context_menu_addon(&mut self, target: ContextMenuTarget) -> Task<RootMessage> {
        let (workshop_id, path) = match target {
            ContextMenuTarget::MyWorkshop { workshop_id, .. } => (Some(workshop_id), None),
            ContextMenuTarget::Local(local) => (local.workshop_id, Some(local.path)),
        };

        self.state.hidden_addons.hide(workshop_id, path.as_deref());
        if let Some(workshop_id) = workshop_id {
            self.state
                .features
                .my_workshop
                .hide_workshop_id(workshop_id);
        }
        self.state
            .features
            .installed_addons
            .hide_addon(workshop_id, path.as_deref());
        self.state
            .features
            .size_analyzer
            .hide_addon(workshop_id, path.as_deref());

        let snapshot = self
            .environment
            .ctx
            .library_snapshot()
            .map(|snapshot| self.state.hidden_addons.visible(&snapshot));
        super::sync_search_installed_addons(&self.environment.ctx, snapshot.as_ref());

        Task::batch([
            self.my_workshop_thumbnail_demands(),
            self.installed_addons_thumbnail_demands(),
            self.size_analyzer_thumbnail_demands(),
            self.search_thumbnail_demands(),
        ])
    }

    pub(super) fn route_local_context_menu_action(
        &mut self,
        action: context_menu::ContextMenuAction,
        target: LocalMenuTarget,
    ) -> Task<RootMessage> {
        use context_menu::ContextMenuAction;

        let LocalMenuTarget {
            path,
            path_text,
            workshop_id,
            workshop_url,
            preview_url,
        } = target;

        match action {
            ContextMenuAction::Extract => self.context_menu_extract_destination_task(vec![path]),
            ContextMenuAction::OpenAddonLocation => self.reveal_path_task(path),
            ContextMenuAction::CopyPath => self.copy_text_task(path_text),
            ContextMenuAction::SteamWorkshop
            | ContextMenuAction::CopyLink
            | ContextMenuAction::Download
            | ContextMenuAction::OpenImage
            | ContextMenuAction::CopyImageLink => self.route_workshop_context_menu_action(
                action,
                workshop_id,
                workshop_url,
                preview_url,
            ),
            #[cfg(feature = "debug")]
            ContextMenuAction::HideAddon
            | ContextMenuAction::AdjustSubscribers(_)
            | ContextMenuAction::SimulateToast(_) => Task::none(),
        }
    }

    pub(super) fn route_workshop_context_menu_action(
        &self,
        action: context_menu::ContextMenuAction,
        workshop_id: Option<PublishedFileId>,
        workshop_url: Option<String>,
        preview_url: Option<String>,
    ) -> Task<RootMessage> {
        use context_menu::ContextMenuAction;

        match action {
            ContextMenuAction::SteamWorkshop => {
                workshop_url.map_or_else(Task::none, |url| self.open_url_task(url))
            }
            ContextMenuAction::CopyLink => {
                workshop_url.map_or_else(Task::none, |url| self.copy_text_task(url))
            }
            ContextMenuAction::Download => workshop_id.map_or_else(Task::none, |id| {
                Task::done(RootMessage::Downloader(
                    downloader::Message::WorkshopIdsSubmitted(vec![id]),
                ))
            }),
            ContextMenuAction::OpenImage => {
                preview_url.map_or_else(Task::none, |url| self.open_url_task(url))
            }
            ContextMenuAction::CopyImageLink => {
                preview_url.map_or_else(Task::none, |url| self.copy_text_task(url))
            }
            ContextMenuAction::Extract
            | ContextMenuAction::OpenAddonLocation
            | ContextMenuAction::CopyPath => Task::none(),
            #[cfg(feature = "debug")]
            ContextMenuAction::HideAddon
            | ContextMenuAction::AdjustSubscribers(_)
            | ContextMenuAction::SimulateToast(_) => Task::none(),
        }
    }

    pub(super) fn reveal_path_task(&self, path: PathBuf) -> Task<RootMessage> {
        self.native_open_target_task("native-reveal-path", NativeOpenTarget::reveal(path))
    }

    pub(super) fn context_menu_extract_destination_task(
        &mut self,
        paths: Vec<PathBuf>,
    ) -> Task<RootMessage> {
        if paths.is_empty() {
            return Task::none();
        }

        // The archive is not opened here, so the file stem stands in for the
        // extracted name.
        let extracted_name = paths
            .first()
            .and_then(|path| path.file_stem())
            .map(|stem| stem.to_string_lossy().into_owned())
            .filter(|stem| !stem.is_empty());
        self.state.context_menu_extraction.begin(paths);
        let context = destination_select::OpenContext {
            confirm_label_key: "destination-extract",
            extracted_name,
            force_create_folder: false,
        };
        self.destination_select_open_task(context)
    }

    /// Context-menu extraction rides the Destination Select overlay:
    /// dismissing it drops the queued paths, a successful save extracts
    /// them, and a failed save leaves the overlay open showing the error.
    pub(super) fn context_menu_destination_dismissed_task(&mut self) -> Task<RootMessage> {
        self.state.context_menu_extraction.clear();
        Task::none()
    }

    pub(super) fn context_menu_destination_persisted_task(&mut self) -> Task<RootMessage> {
        let Some(paths) = self.state.context_menu_extraction.take_paths() else {
            return Task::none();
        };
        self.downloader_local_extraction_task(paths)
    }
}
