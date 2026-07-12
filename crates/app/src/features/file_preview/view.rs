use iced::widget::{
    Space, button, checkbox, column, container, image, progress_bar, row, scrollable, svg, text,
};
use iced::{Border, Center, Color, ContentFit, Element, Font, Length, Shadow, border};

use crate::{
    assets,
    format::format_bytes,
    theme::{self, Tokens, ViewCtx},
    widgets::{
        file_types::{SilkIcon, file_type_info},
        spinner::spinner,
        tooltip as tooltip_widget,
    },
};

use super::model::{
    CodeLine, InfoReason, PreviewContent, PreviewData, PreviewRequest, RelatedPreviewKind,
};
#[cfg(feature = "asset-studio")]
use super::model::{MapStats, ModelPreview, ParticlePreview};
#[cfg(feature = "asset-studio")]
use super::state::MovementMode;
use super::{Message, State};
#[cfg(feature = "asset-studio")]
use gmpublished_backend::particles::SupportLevel;

fn count_text(count: impl std::fmt::Display) -> String {
    count.to_string()
}

const TOOLTIP_MAX_WIDTH: f32 = 280.0;
const SILKICON_SIZE: f32 = 16.0;
const SPINNER_SIZE: f32 = 32.0;
const INFO_LABEL_WIDTH: f32 = 76.0;
const CODE_LINE_NUMBER_WIDTH: f32 = 44.0;
#[cfg(feature = "asset-studio")]
const MODE_PILL_MARGIN: f32 = 12.0;
#[cfg(feature = "asset-studio")]
const MODE_PILL_PADDING: f32 = 3.0;
#[cfg(feature = "asset-studio")]
const MODE_PILL_SLOT_GAP: f32 = 2.0;
#[cfg(feature = "asset-studio")]
const MODE_PILL_SLOT_WIDTH: f32 = 34.0;
#[cfg(feature = "asset-studio")]
const MODE_PILL_SLOT_HEIGHT: f32 = 28.0;
#[cfg(feature = "asset-studio")]
const MODE_PILL_ICON_SIZE: f32 = 18.0;

/// Renders the in-archive File Preview pane embedded in Preview GMA.
pub fn pane<'a>(state: &'a State, ctx: ViewCtx<'a>, show_inspector: bool) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    column![
        header(state, ctx),
        container(body(state, ctx, show_inspector))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(tokens.spacing.pad)
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn header<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let display_name = display_name(state);
    let size_bytes = size_bytes(state);
    let info = file_type_info(display_name);
    let type_label = i18n.trn(
        &format!("file-type-{}", info.type_key),
        &[("arg0", info.extension.as_str())],
    );

    let back = tooltip_widget::below(
        button(icon(
            assets::icons::akar_arrow_left(),
            tokens.colors.text.into(),
            tokens.dims.icon_size,
        ))
        .on_press(Message::BackRequested)
        .padding(tokens.spacing.pad_xs)
        .style(move |_, status| theme::styles::ghost_button(&tokens, status)),
        i18n.tr("file-preview-back"),
        &tokens,
        TOOLTIP_MAX_WIDTH,
    );
    let (expand_icon, expand_tooltip) = if state.expanded() {
        (
            assets::icons::akar_reduce(),
            i18n.tr("file-preview-collapse"),
        )
    } else {
        (
            assets::icons::akar_enlarge(),
            i18n.tr("file-preview-expand"),
        )
    };
    let expand = tooltip_widget::below(
        button(icon(
            expand_icon,
            tokens.colors.text.into(),
            tokens.dims.icon_size,
        ))
        .on_press(Message::ExpandToggled)
        .padding(tokens.spacing.pad_xs)
        .style(move |_, status| theme::styles::ghost_button(&tokens, status)),
        expand_tooltip,
        &tokens,
        TOOLTIP_MAX_WIDTH,
    );

    let name = scrollable(
        text(display_name)
            .size(tokens.typography.title_sm)
            .font(Font {
                weight: iced::font::Weight::Bold,
                ..Font::default()
            })
            .wrapping(text::Wrapping::None),
    )
    .width(Length::Fill)
    .direction(scrollable::Direction::Horizontal(
        theme::styles::hidden_vertical_scrollbar(),
    ));

    let mut controls = row![
        back,
        silk_image(info.icon, type_label, &tokens),
        name,
        text(format_bytes(size_bytes, i18n))
            .size(tokens.typography.body_sm)
            .color(Color::from(tokens.colors.text_dim)),
    ]
    .align_y(Center)
    .spacing(tokens.spacing.gap_sm)
    .height(Length::Fill);

    if let Some(target) = state.related_preview() {
        let tooltip = match target.kind {
            RelatedPreviewKind::Material => i18n.tr("file-preview-open-material"),
            RelatedPreviewKind::Texture => i18n.tr("file-preview-open-texture"),
        };
        controls = controls.push(tooltip_widget::below(
            button(icon(
                assets::icons::context_link_chain(),
                tokens.colors.text.into(),
                tokens.dims.icon_size,
            ))
            .on_press(Message::RelatedPreviewRequested(target.entry_path.clone()))
            .padding(tokens.spacing.pad_xs)
            .style(move |_, status| theme::styles::ghost_button(&tokens, status)),
            tooltip,
            &tokens,
            TOOLTIP_MAX_WIDTH,
        ));
    }

    controls = controls.push(expand);

    if state.extract_entry_path().is_some() {
        controls = controls.push(tooltip_widget::below(
            button(icon(
                assets::icons::akar_download(),
                tokens.colors.text.into(),
                tokens.dims.icon_size,
            ))
            .on_press(Message::ExtractRequested)
            .padding(tokens.spacing.pad_xs)
            .style(move |_, status| theme::styles::ghost_button(&tokens, status)),
            i18n.tr("file-preview-extract"),
            &tokens,
            TOOLTIP_MAX_WIDTH,
        ));
    }
    container(controls)
        .padding([0.0, tokens.spacing.pad])
        .width(Length::Fill)
        .height(Length::Fixed(tokens.dims.control_height))
        .style(move |_| theme::styles::file_preview_header_bar(&tokens, state.expanded()))
        .into()
}

fn body<'a>(state: &'a State, ctx: ViewCtx<'a>, show_inspector: bool) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    if state.loading() {
        let loading_text = state.loading_stage().map_or_else(
            || i18n.tr("file-preview-loading"),
            |stage| i18n.tr(stage.i18n_key()),
        );
        return container(
            column![
                spinner(&tokens, state.spinner_elapsed(), SPINNER_SIZE),
                text(loading_text)
                    .size(tokens.typography.body_sm)
                    .color(Color::from(tokens.colors.text_dim)),
            ]
            .align_x(Center)
            .spacing(tokens.spacing.gap),
        )
        .center(Length::Fill)
        .into();
    }

    if let Some(error) = state.error() {
        let error = error.to_string();
        return container(
            column![
                icon(
                    assets::icons::dead(),
                    tokens.colors.text_dim.into(),
                    SPINNER_SIZE,
                ),
                text(i18n.trn("file-preview-error", &[("arg0", &error)]))
                    .size(tokens.typography.body)
                    .color(Color::from(tokens.colors.text_dim))
                    .align_x(Center)
                    .wrapping(text::Wrapping::WordOrGlyph),
            ]
            .align_x(Center)
            .spacing(tokens.spacing.gap),
        )
        .center(Length::Fill)
        .into();
    }

    state.current().map_or_else(
        || container(Space::new()).center(Length::Fill).into(),
        |data| preview_content(state, data, ctx, show_inspector),
    )
}

#[cfg_attr(not(feature = "asset-studio"), allow(unused_variables))]
fn preview_content<'a>(
    state: &'a State,
    data: &'a PreviewData,
    ctx: ViewCtx<'a>,
    show_inspector: bool,
) -> Element<'a, Message> {
    match &data.content {
        PreviewContent::Code { lines, truncated } => code_preview(lines, *truncated, ctx),
        PreviewContent::Image {
            handle,
            width,
            height,
        } => image_preview(handle, *width, *height, ctx),
        #[cfg(feature = "asset-studio")]
        PreviewContent::Audio { duration_secs, .. } => audio_preview(state, *duration_secs, ctx),
        #[cfg(feature = "asset-studio")]
        PreviewContent::Model(model) => model_preview(state, data, model, ctx, show_inspector),
        #[cfg(feature = "asset-studio")]
        PreviewContent::Particle(preview) => {
            particle_preview(state, data, preview, ctx, show_inspector)
        }
        #[cfg(feature = "asset-studio")]
        PreviewContent::Map {
            scene,
            stats,
            fog,
            sky_camera,
            spawn,
        } => map_preview(
            state,
            data,
            MapPreviewParts {
                scene,
                stats: *stats,
                fog: *fog,
                sky_camera: *sky_camera,
                spawn: *spawn,
            },
            ctx,
            show_inspector,
        ),
        PreviewContent::Info { reason } => info_preview(data, *reason, ctx),
    }
}

fn code_preview<'a>(
    lines: &'a [CodeLine],
    truncated: bool,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let mut rows = column![].width(Length::Fill).spacing(0.0);
    if truncated {
        rows = rows.push(truncation_banner(ctx));
    }
    for (index, line) in lines.iter().enumerate() {
        rows = rows.push(code_line(index + 1, line, &tokens));
    }

    container(
        scrollable(rows)
            .width(Length::Fill)
            .height(Length::Fill)
            .direction(scrollable::Direction::Both {
                vertical: theme::styles::vertical_scrollbar(&tokens),
                horizontal: theme::styles::vertical_scrollbar(&tokens),
            })
            .style(move |_, status| theme::styles::scrollbar(&tokens, status)),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .clip(true)
    .style(move |_| theme::styles::file_preview_body_well(&tokens))
    .into()
}

fn truncation_banner(ctx: ViewCtx<'_>) -> Element<'_, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    container(
        text(i18n.tr("file-preview-truncated"))
            .size(tokens.typography.body_sm)
            .color(Color::from(tokens.colors.text_on_neutral)),
    )
    .width(Length::Fill)
    .padding([tokens.spacing.pad_xs, tokens.spacing.pad_sm])
    .style(move |_| theme::styles::file_preview_banner(&tokens))
    .into()
}

fn code_line<'a>(line_number: usize, line: &'a CodeLine, tokens: &Tokens) -> Element<'a, Message> {
    let number = text(count_text(line_number))
        .size(tokens.typography.caption)
        .font(Font::MONOSPACE)
        .color(Color::from(tokens.colors.text_dim))
        .align_x(iced::alignment::Horizontal::Right)
        .width(Length::Fixed(CODE_LINE_NUMBER_WIDTH));

    row![
        number,
        rich_line(line, tokens)
            .width(Length::Shrink)
            .height(Length::Shrink),
    ]
    .align_y(Center)
    .spacing(tokens.spacing.gap_sm)
    .padding([2.0, tokens.spacing.pad_sm])
    .into()
}

fn rich_line<'a>(line: &'a CodeLine, tokens: &Tokens) -> iced::widget::text::Rich<'a, (), Message> {
    let spans = if line.is_empty() {
        vec![iced::widget::span(" ")]
    } else {
        line.iter()
            .map(|segment| {
                let mut span: iced::widget::text::Span<'a, (), Font> =
                    iced::widget::span(segment.text.as_str());
                if let Some([r, g, b, a]) = segment.color {
                    span = span.color(Color::from_rgba8(r, g, b, f32::from(a) / 255.0));
                }
                span
            })
            .collect::<Vec<_>>()
    };

    iced::widget::rich_text(spans)
        .font(Font::MONOSPACE)
        .size(tokens.typography.body_sm)
        .wrapping(text::Wrapping::None)
}

fn image_preview<'a>(
    handle: &'a iced::widget::image::Handle,
    width: u32,
    height: u32,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let width_text = count_text(width);
    let height_text = count_text(height);
    let caption = i18n.trn(
        "file-preview-image-dimensions",
        &[
            ("arg0", width_text.as_str()),
            ("arg1", height_text.as_str()),
        ],
    );

    column![
        container(
            image(handle.clone())
                .width(Length::Fill)
                .height(Length::Fill)
                .content_fit(ContentFit::Contain)
        )
        .center(Length::Fill)
        .width(Length::Fill)
        .height(Length::Fill)
        .clip(true)
        .style(move |_| theme::styles::file_preview_body_well(&tokens)),
        text(caption)
            .size(tokens.typography.body_sm)
            .color(Color::from(tokens.colors.text_dim))
            .align_x(Center)
            .width(Length::Fill),
    ]
    .spacing(tokens.spacing.gap_sm)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

#[cfg(feature = "asset-studio")]
fn audio_preview<'a>(
    state: &'a State,
    content_duration_secs: Option<f32>,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let playing = state.audio_playing();
    let button_label = if playing {
        i18n.tr("file-preview-audio-pause")
    } else {
        i18n.tr("file-preview-audio-play")
    };
    let duration_secs = state.audio_duration_secs().or(content_duration_secs);
    let position_secs = state.audio_position_secs();
    let progress = duration_secs
        .filter(|duration| *duration > 0.0)
        .map_or(0.0..=1.0, |duration| 0.0..=duration);
    let progress_value = duration_secs
        .filter(|duration| *duration > 0.0)
        .map_or(0.0, |duration| position_secs.clamp(0.0, duration));
    let elapsed = format_audio_time(Some(position_secs));
    let duration = format_audio_time(duration_secs);

    container(
        column![
            image(assets::silkicons::silkicon(SilkIcon::Sound))
                .width(Length::Fixed(48.0))
                .height(Length::Fixed(48.0)),
            button(
                container(
                    text(button_label)
                        .size(tokens.typography.body)
                        .line_height(1.0),
                )
                .center_x(Length::Fill),
            )
            .on_press(Message::AudioToggleRequested)
            .padding(tokens.spacing.pad_control)
            .width(Length::Fill)
            .style(move |_, status| theme::styles::extract_button(&tokens, status)),
            container(
                progress_bar(progress, progress_value)
                    .style(move |_| theme::styles::progress_bar(&tokens)),
            )
            .width(Length::Fill)
            .max_width(360.0),
            text(format!("{elapsed} / {duration}"))
                .size(tokens.typography.body_sm)
                .color(Color::from(tokens.colors.text_dim))
                .align_x(Center),
        ]
        .align_x(Center)
        .spacing(tokens.spacing.gap),
    )
    .center(Length::Fill)
    .padding(tokens.spacing.pad)
    .style(move |_| theme::styles::file_preview_body_well(&tokens))
    .into()
}

#[cfg(feature = "asset-studio")]
fn format_audio_time(seconds: Option<f32>) -> String {
    let Some(seconds) = seconds else {
        return "--:--".to_owned();
    };
    let total = seconds.max(0.0).floor() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

fn info_preview<'a>(
    data: &'a PreviewData,
    reason: InfoReason,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let mut rows = column![
        info_row(
            i18n.tr("file-preview-path"),
            data.entry_path.as_str(),
            &tokens
        ),
        info_row(
            i18n.tr("file-preview-size"),
            format_bytes(data.size_bytes, i18n),
            &tokens
        ),
        info_row(
            i18n.tr("file-preview-crc"),
            format!("{:08X}", data.crc32),
            &tokens
        ),
        info_row(
            i18n.tr("file-preview-reason"),
            i18n.tr(reason_key(reason)),
            &tokens
        ),
        Space::new().height(tokens.spacing.gap),
    ]
    .spacing(tokens.spacing.gap_sm)
    .width(Length::Fill);
    if reason == InfoReason::TooLarge {
        rows = rows.push(
            button(
                container(
                    text(i18n.tr("file-preview-load-anyway"))
                        .size(tokens.typography.body)
                        .line_height(1.0),
                )
                .center_x(Length::Fill),
            )
            .on_press(Message::LoadAnywayRequested)
            .padding(tokens.spacing.pad_control)
            .width(Length::Fill)
            .style(move |_, status| theme::styles::extract_button(&tokens, status)),
        );
    }
    let rows = rows.push(
        button(
            container(
                text(i18n.tr("file-preview-extract"))
                    .size(tokens.typography.body)
                    .line_height(1.0),
            )
            .center_x(Length::Fill),
        )
        .on_press(Message::ExtractRequested)
        .padding(tokens.spacing.pad_control)
        .width(Length::Fill)
        .style(move |_, status| theme::styles::extract_button(&tokens, status)),
    );

    container(rows)
        .width(Length::Fill)
        .padding(tokens.spacing.pad)
        .style(move |_| theme::styles::file_preview_body_well(&tokens))
        .into()
}

#[cfg(feature = "asset-studio")]
fn model_preview<'a>(
    state: &'a State,
    data: &'a PreviewData,
    model: &'a std::sync::Arc<ModelPreview>,
    ctx: ViewCtx<'a>,
    show_inspector: bool,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    // Content-addressed: reopening the same entry reuses the GPU upload.
    let content_id = data.content_id();
    let skin_remap = model
        .skin_tables
        .get(state.selected_skin())
        .cloned()
        .unwrap_or_default();
    let viewer = container(
        iced::widget::shader(super::viewer3d::Viewer3d {
            model: std::sync::Arc::clone(model),
            content_id,
            skin_remap,
            bodygroup_choices: state.bodygroup_choices().to_vec(),
            phy_debug_visible: state.phy_debug_enabled(),
            pose: state.orbit_pose(),
        })
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .clip(true)
    .style(move |_| theme::styles::file_preview_body_well(&tokens));

    if !show_inspector {
        return viewer.into();
    }

    let inspector = container(
        scrollable(
            column![
                model_selectors(state, model, ctx),
                model_inspector_rows(data, model, ctx)
            ]
            .spacing(tokens.spacing.gap_sm),
        )
        .style(move |_, status| theme::styles::scrollbar(&tokens, status)),
    )
    .width(Length::Fixed(tokens.dims.file_preview_inspector_width))
    .height(Length::Fill)
    .padding(tokens.spacing.pad)
    .style(move |_| theme::styles::file_preview_body_well(&tokens));

    row![viewer, inspector]
        .spacing(tokens.spacing.gap_sm)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

#[cfg(feature = "asset-studio")]
#[derive(Clone, Debug, Eq, PartialEq)]
struct IndexedChoice {
    index: usize,
    label: String,
}

#[cfg(feature = "asset-studio")]
impl std::fmt::Display for IndexedChoice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.label)
    }
}

#[cfg(feature = "asset-studio")]
fn model_selectors<'a>(
    state: &'a State,
    model: &'a ModelPreview,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let mut selectors = column![].spacing(tokens.spacing.gap_xs).width(Length::Fill);

    if state.phy_debug_control_visible() {
        selectors = selectors.push(phy_debug_checkbox(state, ctx));
    }

    if model.skin_tables.len() > 1 {
        let options = indexed_choices(model.skin_tables.len(), |index| {
            let index_text = count_text(index);
            i18n.trn(
                "file-preview-model-skin-option",
                &[("arg0", index_text.as_str())],
            )
        });
        let selected = options.get(state.selected_skin()).cloned();
        selectors = selectors.push(selector_row(
            i18n.tr("file-preview-model-skin"),
            options,
            selected,
            |choice| Message::SkinSelected(choice.index),
            &tokens,
        ));
    }

    for (group, &choices) in model.bodygroups.iter().enumerate() {
        if choices < 2 {
            continue;
        }
        let group_text = count_text(group);
        let options = indexed_choices(choices, count_text);
        let selected = state
            .bodygroup_choices()
            .get(group)
            .and_then(|&choice| options.get(choice).cloned());
        selectors = selectors.push(selector_row(
            i18n.trn(
                "file-preview-model-bodygroup",
                &[("arg0", group_text.as_str())],
            ),
            options,
            selected,
            move |choice| Message::BodygroupChoiceSelected {
                group,
                choice: choice.index,
            },
            &tokens,
        ));
    }

    selectors.into()
}

#[cfg(feature = "asset-studio")]
fn indexed_choices(count: usize, label: impl Fn(usize) -> String) -> Vec<IndexedChoice> {
    (0..count)
        .map(|index| IndexedChoice {
            index,
            label: label(index),
        })
        .collect()
}

#[cfg(feature = "asset-studio")]
fn selector_row<'a>(
    label: String,
    options: Vec<IndexedChoice>,
    selected: Option<IndexedChoice>,
    on_selected: impl Fn(IndexedChoice) -> Message + 'a,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let tokens = *tokens;
    row![
        text(label)
            .size(tokens.typography.body_sm)
            .font(Font {
                weight: iced::font::Weight::Bold,
                ..Font::default()
            })
            .width(Length::Fixed(INFO_LABEL_WIDTH)),
        iced::widget::pick_list(options, selected, on_selected)
            .width(Length::Fill)
            .text_size(tokens.typography.body_sm)
            .padding([tokens.spacing.pad_xs, tokens.spacing.pad_sm])
            .style(move |_, status| theme::styles::pick_list(&tokens, status)),
    ]
    .align_y(Center)
    .spacing(tokens.spacing.gap_sm)
    .into()
}

#[cfg(feature = "asset-studio")]
fn model_inspector_rows<'a>(
    data: &'a PreviewData,
    model: &'a ModelPreview,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let stats = model.stats;
    let material_status = material_status_text(stats.resolved_material_count, stats.material_count);
    let rows = column![
        info_row(
            i18n.tr("file-preview-path"),
            data.entry_path.as_str(),
            &tokens
        ),
        info_row(
            i18n.tr("file-preview-size"),
            format_bytes(data.size_bytes, i18n),
            &tokens
        ),
        info_row(
            i18n.tr("file-preview-model-meshes"),
            count_text(stats.mesh_count),
            &tokens
        ),
        info_row(
            i18n.tr("file-preview-model-bones"),
            count_text(stats.bone_count),
            &tokens
        ),
        info_row(
            i18n.tr("file-preview-model-sequences"),
            count_text(stats.sequence_count),
            &tokens
        ),
        info_row(
            i18n.tr("file-preview-model-vertices"),
            count_text(stats.vertex_count),
            &tokens
        ),
        info_row(
            i18n.tr("file-preview-model-triangles"),
            count_text(stats.triangle_count),
            &tokens
        ),
        info_row(
            i18n.tr("file-preview-model-materials"),
            material_status,
            &tokens
        ),
        info_row(
            i18n.tr("file-preview-model-bounds-min"),
            format_model_bounds(model.bounds_min),
            &tokens
        ),
        info_row(
            i18n.tr("file-preview-model-bounds-max"),
            format_model_bounds(model.bounds_max),
            &tokens
        ),
        Space::new().height(tokens.spacing.gap),
        button(
            container(
                text(i18n.tr("file-preview-extract"))
                    .size(tokens.typography.body)
                    .line_height(1.0),
            )
            .center_x(Length::Fill),
        )
        .on_press(Message::ExtractRequested)
        .padding(tokens.spacing.pad_control)
        .width(Length::Fill)
        .style(move |_, status| theme::styles::extract_button(&tokens, status)),
    ]
    .spacing(tokens.spacing.gap_sm)
    .width(Length::Fill);

    rows.into()
}

#[cfg(feature = "asset-studio")]
fn particle_preview<'a>(
    state: &'a State,
    data: &'a PreviewData,
    preview: &'a std::sync::Arc<ParticlePreview>,
    ctx: ViewCtx<'a>,
    show_inspector: bool,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    // Content-addressed: reopening the same entry reuses the GPU upload.
    let content_id = data.content_id();
    let viewer = container(
        iced::widget::shader(super::particles3d::ParticleViewer {
            preview: std::sync::Arc::clone(preview),
            content_id,
            system_index: state.particle_system(),
            playing: state.particle_playing(),
            speed: state.particle_speed(),
            restart_epoch: state.particle_restart_epoch(),
            pose: state.orbit_pose(),
            control_points: state.particle_control_points(),
        })
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .clip(true)
    .style(move |_| theme::styles::file_preview_body_well(&tokens));

    if !show_inspector {
        return viewer.into();
    }

    let inspector = container(
        scrollable(
            column![
                particle_selectors(state, preview, ctx),
                particle_inspector_rows(state, data, preview, ctx)
            ]
            .spacing(tokens.spacing.gap_sm),
        )
        .style(move |_, status| theme::styles::scrollbar(&tokens, status)),
    )
    .width(Length::Fixed(tokens.dims.file_preview_inspector_width))
    .height(Length::Fill)
    .padding(tokens.spacing.pad)
    .style(move |_| theme::styles::file_preview_body_well(&tokens));

    row![viewer, inspector]
        .spacing(tokens.spacing.gap_sm)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

#[cfg(feature = "asset-studio")]
const PARTICLE_SPEED_OPTIONS: [f32; 5] = [0.1, 0.25, 0.5, 1.0, 2.0];

#[cfg(feature = "asset-studio")]
fn particle_speed_choices() -> Vec<IndexedChoice> {
    PARTICLE_SPEED_OPTIONS
        .iter()
        .enumerate()
        .map(|(index, speed)| IndexedChoice {
            index,
            label: format!("{speed}\u{d7}"),
        })
        .collect()
}

#[cfg(feature = "asset-studio")]
fn particle_selectors<'a>(
    state: &'a State,
    preview: &'a ParticlePreview,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let mut selectors = column![].spacing(tokens.spacing.gap_xs).width(Length::Fill);

    if preview.systems.len() > 1 {
        let options = indexed_choices(preview.systems.len(), |index| {
            preview.systems[index].name.clone()
        });
        let selected = options.get(state.particle_system()).cloned();
        selectors = selectors.push(selector_row(
            i18n.tr("file-preview-particle-system"),
            options,
            selected,
            |choice| Message::ParticleSystemSelected(choice.index),
            &tokens,
        ));
    }

    let speed_options = particle_speed_choices();
    let selected_speed = PARTICLE_SPEED_OPTIONS
        .iter()
        .position(|speed| (speed - state.particle_speed()).abs() < 1e-3)
        .and_then(|index| speed_options.get(index).cloned());
    selectors = selectors.push(selector_row(
        i18n.tr("file-preview-particle-speed"),
        speed_options,
        selected_speed,
        |choice| Message::ParticleSpeedSelected(PARTICLE_SPEED_OPTIONS[choice.index]),
        &tokens,
    ));

    let play_label = if state.particle_playing() {
        i18n.tr("file-preview-audio-pause")
    } else {
        i18n.tr("file-preview-audio-play")
    };
    let playback_button = |label: String, message: Message| {
        button(
            container(text(label).size(tokens.typography.body_sm).line_height(1.0))
                .center_x(Length::Fill),
        )
        .on_press(message)
        .padding(tokens.spacing.pad_control)
        .width(Length::Fill)
        .style(move |_, status| theme::styles::extract_button(&tokens, status))
    };
    selectors = selectors.push(
        row![
            playback_button(play_label, Message::ParticlePlayToggled),
            playback_button(
                i18n.tr("file-preview-particle-restart"),
                Message::ParticleRestartRequested
            ),
        ]
        .spacing(tokens.spacing.gap_sm)
        .width(Length::Fill),
    );

    selectors.into()
}

#[cfg(feature = "asset-studio")]
fn particle_inspector_rows<'a>(
    state: &'a State,
    data: &'a PreviewData,
    preview: &'a ParticlePreview,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let resolved = preview
        .materials
        .iter()
        .filter(|slot| slot.texture.is_some())
        .count();
    let mut rows = column![
        info_row(
            i18n.tr("file-preview-path"),
            data.entry_path.as_str(),
            &tokens
        ),
        info_row(
            i18n.tr("file-preview-size"),
            format_bytes(data.size_bytes, i18n),
            &tokens
        ),
        info_row(
            i18n.tr("file-preview-particle-systems"),
            preview.systems.len().to_string(),
            &tokens
        ),
        info_row(
            i18n.tr("file-preview-model-materials"),
            format!("{resolved}/{}", preview.materials.len()),
            &tokens
        ),
    ]
    .spacing(tokens.spacing.gap_sm)
    .width(Length::Fill);

    if let Some(coverage) = preview
        .systems
        .get(state.particle_system())
        .map(|system| &system.coverage)
        .filter(|coverage| !coverage.is_empty())
    {
        rows = rows.push(
            text(i18n.tr("file-preview-particle-fidelity"))
                .size(tokens.typography.body_sm)
                .font(Font {
                    weight: iced::font::Weight::Bold,
                    ..Font::default()
                }),
        );
        for entry in coverage {
            rows = rows.push(coverage_row(entry, ctx));
        }
    }

    rows = rows.push(Space::new().height(tokens.spacing.gap));
    rows = rows.push(
        button(
            container(
                text(i18n.tr("file-preview-extract"))
                    .size(tokens.typography.body)
                    .line_height(1.0),
            )
            .center_x(Length::Fill),
        )
        .on_press(Message::ExtractRequested)
        .padding(tokens.spacing.pad_control)
        .width(Length::Fill)
        .style(move |_, status| theme::styles::extract_button(&tokens, status)),
    );

    rows.into()
}

/// One operator line in the fidelity panel: a support-level dot + name.
#[cfg(feature = "asset-studio")]
fn coverage_row<'a>(
    entry: &'a gmpublished_backend::particles::CoverageEntry,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let (dot_color, level_key) = match entry.level {
        SupportLevel::Full => (
            Color::from_rgb(0.30, 0.78, 0.42),
            "file-preview-particle-level-full",
        ),
        SupportLevel::Approximate => (
            Color::from_rgb(0.95, 0.75, 0.25),
            "file-preview-particle-level-approximate",
        ),
        SupportLevel::PreviewInert => (
            Color::from_rgb(0.55, 0.55, 0.55),
            "file-preview-particle-level-inert",
        ),
        SupportLevel::Unsupported => (
            Color::from_rgb(0.90, 0.35, 0.32),
            "file-preview-particle-level-unsupported",
        ),
    };
    let dot = container(Space::new().width(8.0).height(8.0)).style(move |_| {
        iced::widget::container::Style {
            background: Some(dot_color.into()),
            border: border::rounded(4.0),
            ..iced::widget::container::Style::default()
        }
    });
    tooltip_widget::below(
        row![
            dot,
            text(entry.function.as_str())
                .size(tokens.typography.caption)
                .color(Color::from(tokens.colors.text_dim))
                .wrapping(text::Wrapping::None),
        ]
        .align_y(Center)
        .spacing(tokens.spacing.gap_sm),
        i18n.tr(level_key),
        &tokens,
        TOOLTIP_MAX_WIDTH,
    )
}

#[cfg(feature = "asset-studio")]
#[derive(Clone, Copy)]
struct MapPreviewParts<'a> {
    scene: &'a std::sync::Arc<ModelPreview>,
    stats: MapStats,
    fog: Option<super::model::MapFog>,
    sky_camera: Option<super::model::MapSkyCamera>,
    spawn: Option<super::model::MapSpawn>,
}

#[cfg(feature = "asset-studio")]
fn map_preview<'a>(
    state: &'a State,
    data: &'a PreviewData,
    map: MapPreviewParts<'a>,
    ctx: ViewCtx<'a>,
    show_inspector: bool,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    // Content-addressed: reopening the same map reuses the GPU upload.
    let content_id = data.content_id();
    let viewer = container(
        iced::widget::shader(super::viewer3d::FlyViewer {
            scene: std::sync::Arc::clone(map.scene),
            content_id,
            fog: map.fog,
            fog_enabled: state.map_fog_enabled(),
            sky_camera: map.sky_camera,
            map_skybox_visible: state.map_skybox_enabled(),
            visibility_culling: state.map_visibility_enabled(),
            phy_debug_visible: state.phy_debug_enabled(),
            spawn: map.spawn,
            pose: state.fly_pose(),
            movement_mode: state.fly_movement_mode(),
            requested_movement_mode: state.requested_movement_mode(),
        })
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .clip(true)
    .style(move |_| theme::styles::file_preview_body_well(&tokens));

    let viewer = map_viewer_with_overlays(viewer.into(), state, map, ctx);

    if !show_inspector {
        return viewer;
    }

    let inspector = container(
        scrollable(
            column![
                container(
                    text(i18n.tr("file-preview-map-controls"))
                        .size(tokens.typography.caption)
                        .color(Color::from(tokens.colors.text_dim))
                        .wrapping(text::Wrapping::WordOrGlyph),
                )
                .width(Length::Fill),
                map_inspector_rows(state, data, map.stats, ctx)
            ]
            .spacing(tokens.spacing.gap_sm),
        )
        .style(move |_, status| theme::styles::scrollbar(&tokens, status)),
    )
    .width(Length::Fixed(tokens.dims.file_preview_inspector_width))
    .height(Length::Fill)
    .padding(tokens.spacing.pad)
    .style(move |_| theme::styles::file_preview_body_well(&tokens));

    row![viewer, inspector]
        .spacing(tokens.spacing.gap_sm)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

#[cfg(feature = "asset-studio")]
fn map_viewer_with_overlays<'a>(
    viewer: Element<'a, Message>,
    state: &'a State,
    map: MapPreviewParts<'a>,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let mut layered = viewer;
    if let Some(speed) = state.fly_speed_readout() {
        layered = iced::widget::stack![layered, speed_readout_overlay(speed, &tokens)]
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    }
    if scene_supports_walk(map.scene) {
        let active_mode = active_movement_mode(state, map.spawn);
        layered = iced::widget::stack![layered, mode_pill_overlay(active_mode, ctx)]
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    }
    layered
}

#[cfg(feature = "asset-studio")]
fn speed_readout_overlay<'a>(speed: f32, tokens: &Tokens) -> Element<'a, Message> {
    let tokens = *tokens;
    let readout = container(
        text(format_fly_speed(speed))
            .size(tokens.typography.body_sm)
            .font(Font {
                weight: iced::font::Weight::Bold,
                ..Font::default()
            }),
    )
    .padding([tokens.spacing.pad_xs, tokens.spacing.pad_sm])
    .style(move |_| theme::styles::file_preview_speed_readout(&tokens));
    let overlay = column![
        row![Space::new().width(Length::Fill), readout].width(Length::Fill),
        Space::new().height(Length::Fill),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .padding(tokens.spacing.pad_sm);

    overlay.into()
}

#[cfg(feature = "asset-studio")]
fn mode_pill_overlay(active_mode: MovementMode, ctx: ViewCtx<'_>) -> Element<'_, Message> {
    let pill = mode_pill(active_mode, ctx);
    column![
        Space::new().height(Length::Fill),
        row![pill, Space::new().width(Length::Fill)].width(Length::Fill)
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .padding(MODE_PILL_MARGIN)
    .into()
}

#[cfg(feature = "asset-studio")]
fn mode_pill(active_mode: MovementMode, ctx: ViewCtx<'_>) -> Element<'_, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    container(
        row![
            mode_pill_slot(
                MovementMode::Walk,
                active_mode,
                assets::icons::mode_walk(),
                i18n.tr("file-preview-mode-walk-tooltip"),
                &tokens,
            ),
            mode_pill_slot(
                MovementMode::Fly,
                active_mode,
                assets::icons::mode_fly(),
                i18n.tr("file-preview-mode-fly-tooltip"),
                &tokens,
            ),
        ]
        .spacing(MODE_PILL_SLOT_GAP),
    )
    .padding(MODE_PILL_PADDING)
    .style(|_| mode_pill_style())
    .into()
}

#[cfg(feature = "asset-studio")]
fn mode_pill_slot<'a>(
    mode: MovementMode,
    active_mode: MovementMode,
    icon_handle: iced::widget::svg::Handle,
    tooltip: String,
    tokens: &Tokens,
) -> Element<'a, Message> {
    let active = mode == active_mode;
    let icon_color = Color::from_rgba(1.0, 1.0, 1.0, if active { 1.0 } else { 0.4 });
    let slot = button(
        container(icon(icon_handle, icon_color, MODE_PILL_ICON_SIZE))
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    )
    .width(Length::Fixed(MODE_PILL_SLOT_WIDTH))
    .height(Length::Fixed(MODE_PILL_SLOT_HEIGHT))
    .padding(0)
    .on_press_maybe((!active).then_some(Message::MovementModeSelected(mode)))
    .style(move |_, _| mode_pill_slot_style(active));

    tooltip_widget::below(slot, tooltip, tokens, TOOLTIP_MAX_WIDTH)
}

#[cfg(feature = "asset-studio")]
fn scene_supports_walk(scene: &ModelPreview) -> bool {
    scene
        .walk_collision
        .as_ref()
        .is_some_and(|collision| !collision.is_empty())
}

#[cfg(feature = "asset-studio")]
fn active_movement_mode(state: &State, spawn: Option<super::model::MapSpawn>) -> MovementMode {
    state.fly_movement_mode().unwrap_or_else(|| {
        if spawn.is_some() {
            MovementMode::Walk
        } else {
            MovementMode::Fly
        }
    })
}

#[cfg(feature = "asset-studio")]
fn mode_pill_style() -> iced::widget::container::Style {
    iced::widget::container::Style {
        text_color: Some(Color::WHITE),
        background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.55).into()),
        border: Border {
            color: Color::from_rgba(1.0, 1.0, 1.0, 0.14),
            width: 1.0,
            radius: border::radius(999.0),
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

#[cfg(feature = "asset-studio")]
fn mode_pill_slot_style(active: bool) -> iced::widget::button::Style {
    iced::widget::button::Style {
        background: active.then(|| Color::from_rgba(1.0, 1.0, 1.0, 0.16).into()),
        text_color: Color::WHITE,
        border: Border {
            radius: border::radius(999.0),
            ..Border::default()
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

#[cfg(feature = "asset-studio")]
fn format_fly_speed(speed: f32) -> String {
    format!("{speed:.1}x")
}

#[cfg(feature = "asset-studio")]
fn map_inspector_rows<'a>(
    state: &'a State,
    data: &'a PreviewData,
    stats: MapStats,
    ctx: ViewCtx<'a>,
) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    let material_status = material_status_text(stats.resolved_material_count, stats.material_count);
    let mut rows = column![].spacing(tokens.spacing.gap_sm).width(Length::Fill);
    if state.map_visibility_control_visible() {
        rows = rows.push(
            checkbox(state.map_visibility_enabled())
                .label(i18n.tr("file-preview-map-visibility-culling"))
                .on_toggle(Message::MapVisibilityToggled)
                .text_size(tokens.typography.body)
                .style(move |_, status| theme::styles::checkbox(&tokens, status)),
        );
    }
    if state.map_fog_control_visible() {
        rows = rows.push(
            checkbox(state.map_fog_enabled())
                .label(i18n.tr("file-preview-map-fog"))
                .on_toggle(Message::MapFogToggled)
                .text_size(tokens.typography.body)
                .style(move |_, status| theme::styles::checkbox(&tokens, status)),
        );
    }
    if state.map_skybox_control_visible() {
        rows = rows.push(
            checkbox(state.map_skybox_enabled())
                .label(i18n.tr("file-preview-map-3d-skybox"))
                .on_toggle(Message::MapSkyboxToggled)
                .text_size(tokens.typography.body)
                .style(move |_, status| theme::styles::checkbox(&tokens, status)),
        );
    }
    if state.phy_debug_control_visible() {
        rows = rows.push(phy_debug_checkbox(state, ctx));
    }
    rows = rows.push(info_row(
        i18n.tr("file-preview-path"),
        data.entry_path.as_str(),
        &tokens,
    ));
    let rows = rows
        .push(info_row(
            i18n.tr("file-preview-size"),
            format_bytes(data.size_bytes, i18n),
            &tokens,
        ))
        .push(info_row(
            i18n.tr("file-preview-map-faces"),
            count_text(stats.face_count),
            &tokens,
        ))
        .push(info_row(
            i18n.tr("file-preview-map-displacements"),
            count_text(stats.displacement_count),
            &tokens,
        ))
        .push(info_row(
            i18n.tr("file-preview-map-entities"),
            count_text(stats.entity_count),
            &tokens,
        ))
        .push(info_row(
            i18n.tr("file-preview-map-materials"),
            material_status,
            &tokens,
        ))
        .push(info_row(
            i18n.tr("file-preview-map-version"),
            count_text(stats.version),
            &tokens,
        ))
        .push(Space::new().height(tokens.spacing.gap))
        .push(
            button(
                container(
                    text(i18n.tr("file-preview-extract"))
                        .size(tokens.typography.body)
                        .line_height(1.0),
                )
                .center_x(Length::Fill),
            )
            .on_press(Message::ExtractRequested)
            .padding(tokens.spacing.pad_control)
            .width(Length::Fill)
            .style(move |_, status| theme::styles::extract_button(&tokens, status)),
        );

    rows.into()
}

#[cfg(feature = "asset-studio")]
fn phy_debug_checkbox<'a>(state: &'a State, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    let tokens = *ctx.tokens;
    let i18n = ctx.i18n;
    checkbox(state.phy_debug_enabled())
        .label(i18n.tr("file-preview-map-phy-debug"))
        .on_toggle(Message::PhyDebugToggled)
        .text_size(tokens.typography.body)
        .style(move |_, status| theme::styles::checkbox(&tokens, status))
        .into()
}

#[cfg(feature = "asset-studio")]
fn format_model_bounds(bounds: [f32; 3]) -> String {
    format!("{:.2}, {:.2}, {:.2}", bounds[0], bounds[1], bounds[2])
}

#[cfg(feature = "asset-studio")]
fn material_status_text(resolved: u32, total: u32) -> String {
    format!("{resolved}/{total}")
}

fn info_row<'a>(
    label: impl Into<String>,
    value: impl Into<String>,
    tokens: &Tokens,
) -> Element<'a, Message> {
    row![
        text(label.into())
            .size(tokens.typography.body_sm)
            .font(Font {
                weight: iced::font::Weight::Bold,
                ..Font::default()
            })
            .width(Length::Fixed(INFO_LABEL_WIDTH)),
        text(value.into())
            .size(tokens.typography.body_sm)
            .wrapping(text::Wrapping::WordOrGlyph)
            .width(Length::Fill),
    ]
    .align_y(Center)
    .spacing(tokens.spacing.gap_sm)
    .into()
}

fn reason_key(reason: InfoReason) -> &'static str {
    match reason {
        InfoReason::Binary => "file-preview-reason-binary",
        InfoReason::TooLarge => "file-preview-reason-too-large",
        InfoReason::DecodeFailed => "file-preview-reason-decode-failed",
    }
}

fn current_or_request<'a, T>(
    state: &'a State,
    from_current: impl FnOnce(&'a PreviewData) -> T,
    from_request: impl FnOnce(&'a PreviewRequest) -> T,
) -> Option<T> {
    state
        .current()
        .map(from_current)
        .or_else(|| state.request().map(from_request))
}

fn display_name(state: &State) -> &str {
    current_or_request(
        state,
        |data| data.display_name.as_str(),
        |request| request.display_name.as_str(),
    )
    .unwrap_or_default()
}

fn size_bytes(state: &State) -> u64 {
    current_or_request(state, |data| data.size_bytes, |request| request.size_bytes)
        .unwrap_or_default()
}

fn silk_image<'a>(icon: SilkIcon, tooltip: String, tokens: &Tokens) -> Element<'a, Message> {
    tooltip_widget::below(
        image(assets::silkicons::silkicon(icon))
            .width(Length::Fixed(SILKICON_SIZE))
            .height(Length::Fixed(SILKICON_SIZE)),
        tooltip,
        tokens,
        TOOLTIP_MAX_WIDTH,
    )
}

fn icon<'a>(handle: iced::widget::svg::Handle, color: Color, size: f32) -> Element<'a, Message> {
    svg(handle)
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .style(move |_, _| svg::Style { color: Some(color) })
        .into()
}
