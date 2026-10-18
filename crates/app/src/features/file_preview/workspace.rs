use iced::widget::{Space, button, checkbox, column, container, row, stack, text};
use iced::{Center, Element, Length, Padding};

use crate::media::preview_model::PreviewContent;
use crate::theme::{self, ViewCtx};

use super::{Message, State, view};

pub fn workspace<'a>(
    state: &'a State,
    ctx: ViewCtx<'a>,
    reserved_right: f32,
    reserved_bottom: f32,
) -> Element<'a, Message> {
    let Some(data) = state.current() else {
        let viewer = iced::widget::shader(super::viewer3d::Viewer3d {
            cpu_executor: ctx.cpu_executor.cloned(),
            render_feedback: state.render_feedback().cloned().unwrap_or_default(),
            framing_bounds: None,
            studio_bounds: None,
            animation_pose: None,
            model: empty_studio_model(),
            content_id: u64::MAX,
            skin_remap: Vec::new(),
            bodygroup_choices: Vec::new(),
            phy_debug_visible: false,
            pose: state.orbit_pose(),
            camera_epoch: state.camera_epoch(),
            reserved_space: [reserved_right.max(0.0), reserved_bottom.max(0.0)],
            studio_grid: true,
            live_transform: super::viewer3d::identity_matrix(),
            material_tints: Vec::new(),
        })
        .width(Length::Fill)
        .height(Length::Fill);
        return stack![viewer, view::body(state, ctx, false, 0.0)].into();
    };
    let PreviewContent::Model(model) = &data.content else {
        return view::preview_content(state, data, ctx, false, 0.0);
    };
    let tokens = *ctx.tokens;
    let viewer = iced::widget::shader(super::viewer3d::Viewer3d {
        cpu_executor: ctx.cpu_executor.cloned(),
        render_feedback: state.render_feedback().cloned().unwrap_or_default(),
        framing_bounds: state.model_framing_bounds(),
        studio_bounds: state.model_bounds(),
        animation_pose: state.model_animation_pose(),
        model: std::sync::Arc::clone(model),
        content_id: data.content_id(),
        skin_remap: state.workspace_skin_remap(),
        bodygroup_choices: state.bodygroup_choices().to_vec(),
        phy_debug_visible: state.phy_debug_enabled(),
        pose: state.orbit_pose(),
        camera_epoch: state.camera_epoch(),
        reserved_space: [reserved_right.max(0.0), reserved_bottom.max(0.0)],
        studio_grid: true,
        live_transform: state.workspace_transform(),
        material_tints: state.workspace_material_tints().to_vec(),
    })
    .width(Length::Fill)
    .height(Length::Fill);

    let reset =
        button(text(ctx.i18n.tr("file-preview-reset-camera")).size(tokens.typography.body_sm))
            .on_press(Message::ModelCameraResetRequested)
            .padding(tokens.spacing.pad_control)
            .style(move |_, status| theme::styles::ghost_button(&tokens, status));
    let mut toolbar = row![reset].align_y(Center).spacing(tokens.spacing.gap_sm);
    if state.phy_debug_control_visible() {
        toolbar = toolbar.push(
            checkbox(state.phy_debug_enabled())
                .label(ctx.i18n.tr("file-preview-collision"))
                .on_toggle(Message::PhyDebugToggled)
                .text_size(tokens.typography.body_sm)
                .style(move |_, status| theme::styles::checkbox(&tokens, status)),
        );
    }
    let controls = container(toolbar)
        .padding(tokens.spacing.pad_xs)
        .style(move |_| theme::styles::preview_modal(&tokens));
    let inset = if reserved_bottom > 0.0 {
        tokens.spacing.pad_xs
    } else {
        tokens.spacing.pad
    };
    let overlay = column![
        Space::new().height(Length::Fill),
        row![controls, Space::new().width(Length::Fill)]
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .padding(Padding {
        top: inset,
        right: reserved_right.max(0.0) + inset,
        bottom: reserved_bottom.max(0.0) + inset,
        left: inset,
    });

    container(stack![viewer, overlay])
        .width(Length::Fill)
        .height(Length::Fill)
        .clip(true)
        .style(move |_| theme::styles::file_preview_body_well(&tokens))
        .into()
}

pub fn workspace_selection_controls<'a>(
    state: &'a State,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    state.current_model().map_or_else(
        || Space::new().into(),
        |model| view::model_selection_controls(state, model, ctx, true),
    )
}

pub fn workspace_animation_controls<'a>(
    state: &'a State,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    state.current_model().map_or_else(
        || Space::new().into(),
        |model| {
            if model.animation.as_ref().is_some_and(|animation| {
                animation.as_ref().is_ok_and(|animation| {
                    animation.sequences.len() <= 1
                        && animation.sequences.iter().all(|sequence| {
                            sequence.duration <= 0.0
                                && sequence.parameters.is_empty()
                                && sequence.unavailable.is_none()
                        })
                })
            }) {
                return Space::new().into();
            }
            view::model_animation_controls(state, model, ctx, true)
        },
    )
}

pub fn workspace_stage_controls<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    if state.stage_model().is_none() {
        return Space::new().into();
    }
    let tokens = *ctx.tokens;
    let mut controls = column![
        view::stage_placement_controls(state, ctx),
        view::map_render_controls(state, ctx)
    ]
    .spacing(tokens.spacing.gap);
    if state.stage_model().is_some() && state.phy_debug_control_visible() {
        controls = controls.push(
            checkbox(state.phy_debug_enabled())
                .label(ctx.i18n.tr("file-preview-collision"))
                .on_toggle(Message::PhyDebugToggled)
                .text_size(tokens.typography.body_sm)
                .style(move |_, status| theme::styles::checkbox(&tokens, status)),
        );
    }
    controls.into()
}

fn empty_studio_model() -> std::sync::Arc<crate::media::preview_model::ModelPreview> {
    use crate::media::preview_model::{ModelPreview, ModelStats, RenderScene};
    use gmpublished_domain::math::Vec3;
    use std::sync::{Arc, OnceLock};

    static MODEL: OnceLock<Arc<ModelPreview>> = OnceLock::new();
    Arc::clone(MODEL.get_or_init(|| {
        Arc::new(ModelPreview {
            pose_cache: Default::default(),
            root_lod: 0,
            selected_lod: 0,
            lods: Vec::new(),
            animation: None,
            model_animations: Vec::new(),
            model_animation_issues: Vec::new(),
            scene: Arc::new(RenderScene {
                meshes: Vec::new(),
                materials: Vec::new(),
                phy_debug_meshes: Vec::new(),
                stats: ModelStats {
                    bone_count: 0,
                    sequence_count: 0,
                    vertex_count: 0,
                    triangle_count: 0,
                    mesh_count: 0,
                    material_count: 0,
                    resolved_material_count: 0,
                },
                bounds_min: Vec3::new(-64.0, -64.0, 0.0),
                bounds_max: Vec3::new(64.0, 64.0, 128.0),
            }),
            skin_tables: Vec::new(),
            bodygroups: Vec::new(),
        })
    }))
}
