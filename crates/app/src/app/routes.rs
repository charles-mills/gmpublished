use super::{
    App, RootMessage, Task, addon_grid, downloader, installed_addons, modal_stack, my_workshop,
    shell, size_analyzer,
};

pub(super) fn open_modal_message(modal: modal_stack::ActiveModal) -> modal_stack::Message {
    match modal {
        modal_stack::ActiveModal::DestinationSelect => modal_stack::Message::OpenDestinationSelect,
        modal_stack::ActiveModal::PreparePublish => modal_stack::Message::OpenPreparePublish,
        modal_stack::ActiveModal::PreviewGma => modal_stack::Message::OpenPreviewGma,
        modal_stack::ActiveModal::Settings => modal_stack::Message::OpenSettings,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RouteLifecycle {
    Entered,
    Exited,
}

impl RouteLifecycle {
    const fn my_workshop_message(self) -> my_workshop::Message {
        match self {
            Self::Entered => my_workshop::Message::RouteEntered,
            Self::Exited => my_workshop::Message::RouteExited,
        }
    }

    const fn installed_addons_message(self) -> installed_addons::Message {
        match self {
            Self::Entered => installed_addons::Message::RouteEntered,
            Self::Exited => installed_addons::Message::RouteExited,
        }
    }

    const fn downloader_message(self) -> downloader::Message {
        match self {
            Self::Entered => downloader::Message::RouteEntered,
            Self::Exited => downloader::Message::RouteExited,
        }
    }

    const fn size_analyzer_message(self) -> size_analyzer::Message {
        match self {
            Self::Entered => size_analyzer::Message::RouteEntered,
            Self::Exited => size_analyzer::Message::RouteExited,
        }
    }
}

impl App {
    pub(super) fn route_transitioned_task(
        &mut self,
        previous: shell::Route,
        next: shell::Route,
    ) -> Task<RootMessage> {
        if previous == next {
            debug_assert_ne!(
                previous, next,
                "route_transitioned_task is an invariant guard; shell emits Navigated only on changes"
            );
            return Task::none();
        }

        Task::batch([
            self.route_lifecycle_task(previous, RouteLifecycle::Exited),
            self.route_lifecycle_task(next, RouteLifecycle::Entered),
        ])
    }

    pub(super) fn route_lifecycle_task(
        &mut self,
        route: shell::Route,
        lifecycle: RouteLifecycle,
    ) -> Task<RootMessage> {
        match route {
            shell::Route::MyWorkshop => {
                let task = self.apply_my_workshop_message(lifecycle.my_workshop_message());
                match lifecycle {
                    RouteLifecycle::Entered => Task::batch([
                        task,
                        grid_scroll_restore(
                            my_workshop::GRID_KEY,
                            self.state.my_workshop.grid().scroll_offset(),
                        ),
                    ]),
                    RouteLifecycle::Exited => task,
                }
            }
            shell::Route::InstalledAddons => {
                let task =
                    self.apply_installed_addons_message(lifecycle.installed_addons_message());
                match lifecycle {
                    RouteLifecycle::Entered => Task::batch([
                        task,
                        grid_scroll_restore(
                            installed_addons::GRID_KEY,
                            self.state.installed_addons.grid().scroll_offset(),
                        ),
                    ]),
                    RouteLifecycle::Exited => task,
                }
            }
            shell::Route::Downloader => {
                self.apply_downloader_message(lifecycle.downloader_message())
            }
            shell::Route::SizeAnalyzer => {
                self.apply_size_analyzer_message(lifecycle.size_analyzer_message())
            }
        }
    }
}

/// My Workshop and Installed Addons render structurally identical trees, so
/// the scrollable's widget state is shared between them by Iced's positional
/// tree diff: switching routes leaves the viewport wherever the *other*
/// grid was scrolled. Entering a grid route snaps the widget back to this
/// route's own remembered offset.
fn grid_scroll_restore(key: &'static str, offset: f32) -> Task<RootMessage> {
    iced::widget::operation::scroll_to(
        addon_grid::scrollable_id(key),
        iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: offset },
    )
}
