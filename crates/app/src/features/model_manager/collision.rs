use std::path::PathBuf;

use gmpublished_backend::model_compile::{CollisionDetail, ModelCollision, ModelInspection};
use iced::widget::{button, checkbox, column, text, text_input};
use iced::{Element, Length};

use super::{Effect, Message, SourceKind, State};
use crate::generation::Generation;
use crate::theme::{self, ViewCtx};
use crate::widgets::select;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CollisionMode {
    Automatic,
    Custom,
    #[default]
    Existing,
    None,
}

impl CollisionMode {
    fn label(self) -> &'static str {
        match self {
            Self::Automatic => "models-collision-automatic",
            Self::Custom => "models-collision-custom",
            Self::Existing => "models-collision-existing",
            Self::None => "models-collision-none",
        }
    }
}

#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "tracks independent edits so asynchronous inspection cannot replace user input"
)]
pub(super) struct Editor {
    pub mode: CollisionMode,
    detail: CollisionDetail,
    repair_custom: bool,
    source: Option<PathBuf>,
    parts: Vec<String>,
    inspection: Option<ModelInspection>,
    inspection_generation: Option<Generation>,
    filter: String,
    error: Option<String>,
    edited: bool,
    mass_edited: bool,
    surface_edited: bool,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            mode: CollisionMode::Existing,
            detail: CollisionDetail::Balanced,
            repair_custom: false,
            source: None,
            parts: Vec::new(),
            inspection: None,
            inspection_generation: None,
            filter: String::new(),
            error: None,
            edited: false,
            mass_edited: false,
            surface_edited: false,
        }
    }
}

impl Editor {
    pub(super) fn for_kind(kind: SourceKind) -> Self {
        Self {
            mode: if kind == SourceKind::Mesh {
                CollisionMode::Automatic
            } else {
                CollisionMode::Existing
            },
            ..Self::default()
        }
    }

    pub(super) fn generates_physics(&self) -> bool {
        matches!(self.mode, CollisionMode::Automatic | CollisionMode::Custom)
    }
}

enum CollisionSelection<'a> {
    Automatic(CollisionDetail),
    Existing,
    None,
    Custom {
        source: &'a std::path::Path,
        parts: &'a [String],
        repair: Option<CollisionDetail>,
    },
}

impl State {
    pub(super) fn collision_valid(&self) -> bool {
        self.collision_selection().is_some()
    }

    pub(super) fn collision_request(&self) -> Option<ModelCollision> {
        Some(match self.collision_selection()? {
            CollisionSelection::Automatic(detail) => ModelCollision::Automatic(detail),
            CollisionSelection::Existing => ModelCollision::Existing,
            CollisionSelection::None => ModelCollision::None,
            CollisionSelection::Custom {
                source,
                parts,
                repair,
            } => ModelCollision::Custom {
                source: source.to_path_buf(),
                parts: parts.to_vec(),
                repair,
            },
        })
    }

    fn collision_selection(&self) -> Option<CollisionSelection<'_>> {
        if self.kind() != Some(SourceKind::Mesh) {
            return Some(if self.collision.mode == CollisionMode::None {
                CollisionSelection::None
            } else {
                CollisionSelection::Existing
            });
        }
        if !self.collision.edited
            && self.inspection.is_none()
            && self.inspection_generation.is_some()
            && self.source.as_ref().is_some_and(|path| {
                path.extension().is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("gltf") || extension.eq_ignore_ascii_case("glb")
                })
            })
        {
            return None;
        }
        Some(match self.collision.mode {
            CollisionMode::Automatic => CollisionSelection::Automatic(self.collision.detail),
            CollisionMode::Existing => {
                if let Some(inspection) = &self.inspection
                    && inspection.source_model.is_none()
                    && !inspection.collision_parts.is_empty()
                {
                    CollisionSelection::Custom {
                        source: self.source.as_deref()?,
                        parts: &inspection.collision_parts,
                        repair: None,
                    }
                } else {
                    CollisionSelection::Existing
                }
            }
            CollisionMode::None => CollisionSelection::None,
            CollisionMode::Custom => {
                let source = self.collision.source.as_ref()?;
                if self.collision.inspection_generation.is_some() || self.collision.error.is_some()
                {
                    return None;
                }
                let inspection = if self.source.as_ref() == Some(source) {
                    self.inspection.as_ref()?
                } else {
                    self.collision.inspection.as_ref()?
                };
                if ((self.source.as_ref() == Some(source) || !inspection.parts.is_empty())
                    && self.collision.parts.is_empty())
                    || !self.collision.parts.iter().all(|selector| {
                        inspection
                            .parts
                            .iter()
                            .any(|part| &part.selector == selector)
                    })
                    || (self.source.as_ref() == Some(source)
                        && inspection
                            .parts
                            .iter()
                            .all(|part| self.collision.parts.contains(&part.selector)))
                {
                    return None;
                }
                CollisionSelection::Custom {
                    source: source.as_path(),
                    parts: &self.collision.parts,
                    repair: self
                        .collision
                        .repair_custom
                        .then_some(self.collision.detail),
                }
            }
        })
    }

    pub(super) fn collision_generates_physics(&self) -> bool {
        self.collision.generates_physics()
            || (self.collision.mode == CollisionMode::Existing
                && self
                    .inspection
                    .as_ref()
                    .is_some_and(|inspection| !inspection.collision_parts.is_empty()))
    }

    pub(super) fn is_collision_part(&self, selector: &str) -> bool {
        self.inspection.as_ref().is_some_and(|inspection| {
            inspection
                .collision_parts
                .iter()
                .any(|part| part == selector)
        }) || (self.collision.mode == CollisionMode::Custom
            && self.source == self.collision.source
            && self.collision.parts.iter().any(|part| part == selector))
    }
}

pub(super) fn update(state: &mut State, message: &Message) -> Option<Vec<Effect>> {
    if let Message::CollisionSourceInspected(generation, result) = message {
        if state.collision.inspection_generation == Some(*generation) {
            state.collision.inspection_generation = None;
            match result {
                Ok(inspection) => {
                    state.collision.parts = inspection
                        .parts
                        .iter()
                        .map(|part| part.selector.clone())
                        .collect();
                    state.collision.inspection = Some(inspection.as_ref().clone());
                }
                Err(error) => state.collision.error = Some(error.clone()),
            }
        }
        return Some(Vec::new());
    }
    if state.busy() || state.kind().is_none_or(|kind| kind == SourceKind::Compiled) {
        return None;
    }
    match message {
        Message::CollisionMassChanged(_) => {
            state.collision.mass_edited = true;
            return None;
        }
        Message::SurfacePropertyChanged(_) => {
            state.collision.surface_edited = true;
            return None;
        }
        Message::CollisionModeChanged(mode)
            if state.kind() == Some(SourceKind::Mesh)
                || matches!(mode, CollisionMode::Existing | CollisionMode::None) =>
        {
            state.collision.mode = *mode;
            state.collision.edited = true;
        }
        Message::CollisionRepairChanged(enabled)
            if state.collision.mode == CollisionMode::Custom =>
        {
            state.collision.repair_custom = *enabled;
            state.collision.edited = true;
        }
        Message::CollisionDetailChanged(detail) if state.kind() == Some(SourceKind::Mesh) => {
            state.collision.detail = *detail;
            state.collision.edited = true;
        }
        Message::CollisionSourceRequested if state.kind() == Some(SourceKind::Mesh) => {
            let generation = state.generation.bump();
            state.picker = Some(generation);
            return Some(vec![Effect::PickCollisionSource(
                generation,
                state
                    .source
                    .as_ref()
                    .and_then(|source| source.parent())
                    .map(std::path::Path::to_path_buf),
            )]);
        }
        Message::CollisionUseSource if state.kind() == Some(SourceKind::Mesh) => {
            state.collision.mode = CollisionMode::Custom;
            state.collision.repair_custom = false;
            state.collision.source.clone_from(&state.source);
            state.collision.parts.clear();
            state.collision.inspection = None;
            state.collision.inspection_generation = None;
            state.collision.error = None;
            state.collision.edited = true;
        }
        Message::CollisionPartToggled(selector, selected) => {
            let inspection = if state.source == state.collision.source {
                state.inspection.as_ref()
            } else {
                state.collision.inspection.as_ref()
            };
            if inspection.is_none_or(|inspection| {
                !inspection
                    .parts
                    .iter()
                    .any(|part| &part.selector == selector)
            }) {
                return Some(Vec::new());
            }
            if *selected {
                if state.source == state.collision.source && state.bodygroups.iter().any(|group| {
                    group.choices.iter().any(|choice| matches!(choice, gmpublished_backend::model_compile::BodygroupChoice::Mesh {parts,..} if parts.contains(selector)))
                }) {
                    return Some(Vec::new());
                }
                if !state.collision.parts.contains(selector) {
                    state.collision.parts.push(selector.clone());
                }
            } else {
                state.collision.parts.retain(|part| part != selector);
            }
            state.collision.edited = true;
        }
        Message::CollisionFilterChanged(filter) => {
            state.collision.filter.clone_from(filter);
            return Some(Vec::new());
        }
        _ => return None,
    }
    super::update::clear_outcome(state);
    Some(Vec::new())
}

pub(super) fn inspected(state: &mut State, inspection: &ModelInspection) {
    if state.kind() == Some(SourceKind::Mesh)
        && !state.collision.edited
        && inspection.source_model.is_some()
    {
        state.collision.mode = CollisionMode::Existing;
    }
    if state.kind() == Some(SourceKind::Mesh)
        && let Some(physics) = &inspection.collision_physics
    {
        if !state.collision.mass_edited {
            state.collision_mass = physics.mass.to_string();
        }
        if !state.collision.surface_edited {
            state.surface_property = physics.surface_property.clone().unwrap_or_default();
        }
    }
    if state.kind() == Some(SourceKind::Mesh)
        && !state.collision.edited
        && inspection.source_model.is_none()
        && !inspection.collision_parts.is_empty()
    {
        state.collision.mode = CollisionMode::Custom;
        state.collision.source.clone_from(&state.source);
        state
            .collision
            .parts
            .clone_from(&inspection.collision_parts);
    }
}

pub(super) fn source_selected(
    state: &mut State,
    generation: Generation,
    path: Option<PathBuf>,
) -> Option<Vec<Effect>> {
    if state.picker != Some(generation) {
        return None;
    }
    state.picker = None;
    let Some(path) = path else {
        return Some(Vec::new());
    };
    if SourceKind::of(&path) != Some(SourceKind::Mesh) {
        return Some(Vec::new());
    }
    let same_source = state.source.as_ref().is_some_and(|source| {
        source == &path || matches!((std::fs::canonicalize(source), std::fs::canonicalize(&path)), (Ok(first), Ok(second)) if first == second)
    });
    state.collision.mode = CollisionMode::Custom;
    state.collision.repair_custom = false;
    state.collision.source = if same_source {
        state.source.clone()
    } else {
        Some(path.clone())
    };
    state.collision.parts.clear();
    state.collision.inspection = None;
    state.collision.error = None;
    state.collision.edited = true;
    super::update::clear_outcome(state);
    if same_source {
        state.collision.inspection_generation = None;
        Some(Vec::new())
    } else {
        let inspection_generation = state.generation.bump();
        state.collision.inspection_generation = Some(inspection_generation);
        Some(vec![Effect::InspectCollision(inspection_generation, path)])
    }
}

pub(super) fn view<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let editor = &state.collision;
    let busy = state.busy();
    let modes = if state.kind() == Some(SourceKind::Mesh) {
        vec![
            CollisionMode::Automatic,
            CollisionMode::Custom,
            CollisionMode::Existing,
            CollisionMode::None,
        ]
    } else {
        vec![CollisionMode::Existing, CollisionMode::None]
    };
    let mut content = column![
        text(i18n.tr("models-collision")).size(tokens.typography.body_lg),
        choice(
            modes
                .into_iter()
                .map(|mode| (
                    i18n.tr(mode.label()),
                    Message::CollisionModeChanged(mode),
                    mode == editor.mode
                ))
                .collect(),
            i18n.tr(editor.mode.label()),
            busy,
            ctx
        ),
    ]
    .spacing(tokens.spacing.gap);
    match editor.mode {
        CollisionMode::Automatic => {
            content = content.push(detail_control(editor.detail, busy, ctx));
            content = content.push(
                text(i18n.tr("models-collision-automatic-hint"))
                    .size(tokens.typography.body_sm)
                    .color(tokens.colors.text_dim),
            );
        }
        CollisionMode::Custom => {
            content = content.push(
                column![
                    button(text(i18n.tr("models-collision-custom-file")))
                        .on_press_maybe((!busy).then_some(Message::CollisionSourceRequested))
                        .style(move |_, status| theme::styles::button(&tokens, status)),
                    button(text(i18n.tr("models-collision-use-source")))
                        .on_press_maybe((!busy).then_some(Message::CollisionUseSource))
                        .style(move |_, status| theme::styles::button(&tokens, status)),
                ]
                .spacing(tokens.spacing.gap),
            );
            if let Some(source) = &editor.source {
                content = content
                    .push(text(source.display().to_string()).size(tokens.typography.body_sm));
            }
            let inspection = if state.source == editor.source {
                state.inspection.as_ref()
            } else {
                editor.inspection.as_ref()
            };
            if let Some(inspection) = inspection {
                content = content.push(
                    text_input(&i18n.tr("models-bodygroup-search"), &editor.filter)
                        .on_input_maybe((!busy).then_some(Message::CollisionFilterChanged))
                        .padding(tokens.spacing.pad_control)
                        .style(move |_, status| theme::styles::input(&tokens, status)),
                );
                let filter = editor.filter.to_lowercase();
                let mut name_counts = std::collections::HashMap::new();
                for part in &inspection.parts {
                    *name_counts.entry(part.name.as_str()).or_insert(0usize) += 1;
                }
                let matching: Vec<_> = inspection
                    .parts
                    .iter()
                    .enumerate()
                    .map(|(index, part)| {
                        let label = if name_counts[part.name.as_str()] > 1 {
                            format!("{} ({})", part.name, index + 1)
                        } else {
                            part.name.clone()
                        };
                        (part, label)
                    })
                    .filter(|(_, label)| {
                        filter.is_empty() || label.to_lowercase().contains(&filter)
                    })
                    .collect();
                for (part, label) in matching.iter().take(100) {
                    let selector = part.selector.clone();
                    let available = state.source != editor.source || !state.bodygroups.iter().any(|group| group.choices.iter().any(|choice| matches!(choice,gmpublished_backend::model_compile::BodygroupChoice::Mesh {parts,..} if parts.contains(&selector))));
                    content = content.push(
                        checkbox(editor.parts.contains(&selector))
                            .label(label.clone())
                            .text_size(tokens.typography.body_sm)
                            .width(Length::Fill)
                            .on_toggle_maybe((!busy && available).then_some(move |selected| {
                                Message::CollisionPartToggled(selector.clone(), selected)
                            }))
                            .style(move |_, status| theme::styles::checkbox(&tokens, status)),
                    );
                }
                if matching.len() > 100 {
                    content = content.push(
                        text(i18n.tr("models-bodygroup-search-more"))
                            .size(tokens.typography.body_sm),
                    );
                }
            }
            if editor.inspection_generation.is_some() {
                content = content.push(
                    text(i18n.tr("models-collision-loading")).size(tokens.typography.body_sm),
                );
            } else if let Some(error) = &editor.error {
                content = content.push(
                    text(error)
                        .size(tokens.typography.body_sm)
                        .color(tokens.colors.error),
                );
            } else if !state.collision_valid() {
                content = content.push(
                    text(i18n.tr("models-collision-select-parts"))
                        .size(tokens.typography.body_sm)
                        .color(tokens.colors.error),
                );
            }
            content = content.push(
                checkbox(editor.repair_custom)
                    .label(i18n.tr("models-collision-repair"))
                    .on_toggle_maybe((!busy).then_some(Message::CollisionRepairChanged))
                    .style(move |_, status| theme::styles::checkbox(&tokens, status)),
            );
            if editor.repair_custom {
                content = content.push(detail_control(editor.detail, busy, ctx));
            }
            content = content.push(
                text(i18n.tr(if editor.repair_custom {
                    "models-collision-repair-hint"
                } else {
                    "models-collision-custom-hint"
                }))
                .size(tokens.typography.body_sm)
                .color(tokens.colors.text_dim),
            );
        }
        CollisionMode::Existing => {
            content = content.push(
                text(i18n.tr("models-collision-existing-hint"))
                    .size(tokens.typography.body_sm)
                    .color(tokens.colors.text_dim),
            );
        }
        CollisionMode::None => {}
    }
    if state.collision_generates_physics() {
        content = content
            .push(text(i18n.tr("models-collision-mass")).size(tokens.typography.body_sm))
            .push(
                text_input("1", &state.collision_mass)
                    .on_input_maybe((!busy).then_some(Message::CollisionMassChanged))
                    .padding(tokens.spacing.pad_control)
                    .width(Length::Fill)
                    .style(move |_, status| theme::styles::input(&tokens, status)),
            );
        if state.parsed_collision_mass().is_none() {
            content = content.push(
                text(i18n.tr("models-invalid-mass"))
                    .size(tokens.typography.body_sm)
                    .color(tokens.colors.error),
            );
        }
    }
    content.into()
}

fn detail_control(detail: CollisionDetail, busy: bool, ctx: ViewCtx<'_>) -> Element<'_, Message> {
    let choices = [
        (CollisionDetail::Simple, "models-collision-simple"),
        (CollisionDetail::Balanced, "models-collision-balanced"),
        (CollisionDetail::Detailed, "models-collision-detailed"),
    ];
    let label = ctx.i18n.tr(match detail {
        CollisionDetail::Simple => "models-collision-simple",
        CollisionDetail::Balanced => "models-collision-balanced",
        CollisionDetail::Detailed => "models-collision-detailed",
    });
    column![
        text(ctx.i18n.tr("models-collision-detail")).size(ctx.tokens.typography.body_sm),
        choice(
            choices
                .into_iter()
                .map(|(value, key)| (
                    ctx.i18n.tr(key),
                    Message::CollisionDetailChanged(value),
                    value == detail
                ))
                .collect(),
            label,
            busy,
            ctx
        ),
    ]
    .spacing(ctx.tokens.spacing.gap)
    .into()
}

fn choice(
    rows: Vec<(String, Message, bool)>,
    label: String,
    busy: bool,
    ctx: ViewCtx<'_>,
) -> Element<'_, Message> {
    if busy {
        return text(label).size(ctx.tokens.typography.body).into();
    }
    select::field(
        label,
        rows.into_iter()
            .map(|(label, on_select, selected)| select::Row {
                label,
                on_select,
                selected,
            })
            .collect(),
        select::Metrics {
            text_size: ctx.tokens.typography.body,
            padding: iced::Padding::from(ctx.tokens.spacing.pad_control),
            radius: ctx.tokens.radii.sm,
            max_rows: select::MAX_ROWS,
        },
        select::Chevron::Shown,
        ctx.tokens,
    )
}

#[cfg(test)]
mod tests;
