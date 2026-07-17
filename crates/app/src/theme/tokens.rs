use crate::bridge::{EffectiveThemePreset, ThemePreset, theme as core_theme};
use iced::{Color, Theme, theme::Palette};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThemeVariant {
    Dark,
    Light,
    ClassicSource,
}

impl ThemeVariant {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Dark => "gmpublished Dark",
            Self::Light => "gmpublished Light",
            Self::ClassicSource => "gmpublished Classic Source",
        }
    }

    const fn preset(self) -> ThemePreset {
        match self {
            Self::Dark => ThemePreset::Dark,
            Self::Light => ThemePreset::Light,
            Self::ClassicSource => ThemePreset::ClassicSource,
        }
    }
}

impl From<EffectiveThemePreset> for ThemeVariant {
    fn from(value: EffectiveThemePreset) -> Self {
        match value {
            EffectiveThemePreset::Dark => Self::Dark,
            EffectiveThemePreset::Light => Self::Light,
            EffectiveThemePreset::ClassicSource => Self::ClassicSource,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccentInputs {
    pub neutral: u32,
    pub success: u32,
    pub error: u32,
}

impl AccentInputs {
    pub(crate) const fn for_preset(preset: ThemePreset) -> Self {
        let (neutral, success, error) = preset.accent_colors();
        Self {
            neutral,
            success,
            error,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn rgb(rgb: u32) -> Self {
        Self {
            r: ((rgb & 0xFF0000) >> 16) as u8,
            g: ((rgb & 0x00FF00) >> 8) as u8,
            b: (rgb & 0x0000FF) as u8,
            a: 255,
        }
    }

    pub const fn from_rgba(rgb: u32, alpha: u8) -> Self {
        Self {
            r: ((rgb & 0xFF0000) >> 16) as u8,
            g: ((rgb & 0x00FF00) >> 8) as u8,
            b: (rgb & 0x0000FF) as u8,
            a: alpha,
        }
    }

    #[must_use]
    pub const fn with_alpha(self, alpha: u8) -> Self {
        Self { a: alpha, ..self }
    }
}

impl From<Rgba> for Color {
    fn from(value: Rgba) -> Self {
        Self::from_rgba8(value.r, value.g, value.b, f32::from(value.a) / 255.0)
    }
}

impl From<core_theme::Rgb> for Rgba {
    fn from(value: core_theme::Rgb) -> Self {
        Self {
            r: value.r,
            g: value.g,
            b: value.b,
            a: 255,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Colors {
    pub(crate) neutral: Rgba,
    pub(crate) neutral_dark: Rgba,
    pub(crate) success: Rgba,
    pub(crate) success_dark: Rgba,
    pub(crate) star_rating_filled: Rgba,
    pub(crate) star_rating_empty: Rgba,
    pub(crate) download_count_icon: Rgba,
    pub(crate) error: Rgba,
    pub(crate) error_dark: Rgba,
    pub(crate) link: Rgba,
    pub(crate) text: Rgba,
    pub(crate) text_dim: Rgba,
    pub(crate) text_watermark: Rgba,
    pub(crate) text_inverted: Rgba,
    pub(crate) text_on_neutral: Rgba,
    pub(crate) text_on_success: Rgba,
    pub(crate) text_on_error: Rgba,
    pub(crate) bg: Rgba,
    pub(crate) sidebar_panel_bg: Rgba,
    pub(crate) surface: Rgba,
    pub(crate) surface_2: Rgba,
    pub(crate) surface_raised: Rgba,
    pub(crate) surface_muted: Rgba,
    pub(crate) surface_sunken: Rgba,
    pub(crate) surface_deep: Rgba,
    pub(crate) surface_preview: Rgba,
    pub(crate) surface_preview_card: Rgba,
    pub(crate) chrome_deep: Rgba,
    pub(crate) sidebar_item_hover: Rgba,
    pub(crate) input_bg: Rgba,
    pub(crate) search_input_bg: Rgba,
    pub(crate) search_scrim: Rgba,
    pub(crate) overlay_panel_bg: Rgba,
    pub(crate) overlay_divider: Rgba,
    pub(crate) search_keycap_border: Rgba,
    pub(crate) account_update_bg: Rgba,
    pub(crate) account_update_hover_bg: Rgba,
    pub(crate) destination_input_bg: Rgba,
    pub(crate) tile_disabled_bg: Rgba,
    pub(crate) tile_disabled_text: Rgba,
    pub(crate) row_stripe: Rgba,
    pub(crate) browser_empty_dim: Rgba,
    pub(crate) tooltip_bg: Rgba,
    pub(crate) menu_bg: Rgba,
    pub(crate) dropdown_bg: Rgba,
    pub(crate) modal_bg: Rgba,
    pub(crate) preview_modal_bg: Rgba,
    pub(crate) extract_disabled_bg: Rgba,
    pub(crate) browser_shortcut_dim: Rgba,
    pub(crate) menu_option_selected_bg: Rgba,
    pub(crate) menu_option_selected_text: Rgba,
    pub(crate) button_bg: Rgba,
    pub(crate) button_pressed: Rgba,
    pub(crate) control_bg: Rgba,
    pub(crate) control_bg_alt: Rgba,
    pub(crate) control_pressed: Rgba,
    pub(crate) switch_on: Rgba,
    pub(crate) switch_off: Rgba,
    pub(crate) switch_knob: Rgba,
    pub(crate) icon_muted: Rgba,
    pub(crate) border: Rgba,
    pub(crate) border_strong: Rgba,
    pub(crate) border_subtle: Rgba,
    pub(crate) divider: Rgba,
    pub(crate) divider_strong: Rgba,
    pub(crate) checkbox_border: Rgba,
    pub(crate) focus_ring: Rgba,
    pub(crate) hover_fill: Rgba,
    pub(crate) hover_fill_medium: Rgba,
    pub(crate) hover_fill_subtle: Rgba,
    pub(crate) hover_fill_soft: Rgba,
    pub(crate) hover_fill_faint: Rgba,
    pub(crate) row_hover_fill: Rgba,
    pub(crate) row_hover_fill_strong: Rgba,
    pub(crate) row_fill: Rgba,
    pub(crate) row_fill_subtle: Rgba,
    pub(crate) row_fill_alt: Rgba,
    pub(crate) row_fill_soft: Rgba,
    pub(crate) row_fill_medium: Rgba,
    pub(crate) selected_fill: Rgba,
    pub(crate) selected_fill_strong: Rgba,
    pub(crate) scrollbar_grabber: Rgba,
    pub(crate) scrollbar_grabber_hover: Rgba,
    pub(crate) scrollbar_grabber_active: Rgba,
    pub(crate) scrollbar_rail: Rgba,
    pub(crate) scrim: Rgba,
    pub(crate) scrim_strong: Rgba,
    pub(crate) scrim_soft: Rgba,
    pub(crate) scrim_expanded: Rgba,
    pub(crate) overlay_fill: Rgba,
    pub(crate) overlay_fill_soft: Rgba,
    pub(crate) shadow: Rgba,
    pub(crate) shadow_soft: Rgba,
    pub(crate) shadow_control: Rgba,
    pub(crate) shadow_action: Rgba,
    pub(crate) shadow_raised: Rgba,
    pub(crate) shadow_card: Rgba,
    pub(crate) shadow_strong: Rgba,
    pub(crate) shadow_card_strong: Rgba,
    pub(crate) shadow_dropdown: Rgba,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spacing {
    pub(crate) gap_xs: f32,
    pub(crate) gap_sm: f32,
    pub(crate) gap_md: f32,
    pub(crate) gap: f32,
    pub(crate) gap_lg: f32,
    pub(crate) pad_xs: f32,
    pub(crate) pad_sm: f32,
    pub(crate) pad_control: f32,
    pub(crate) pad_control_x: f32,
    pub(crate) pad_control_y: f32,
    pub(crate) pad: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Radii {
    pub(crate) xs: f32,
    pub(crate) base: f32,
    pub(crate) md: f32,
    pub(crate) lg: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Typography {
    pub(crate) caption_xs: f32,
    pub(crate) caption: f32,
    pub(crate) body_sm: f32,
    pub(crate) body: f32,
    pub(crate) body_lg: f32,
    pub(crate) title_xs: f32,
    pub(crate) title_sm: f32,
    pub(crate) title: f32,
    pub(crate) title_lg: f32,
    pub(crate) display_xs: f32,
    pub(crate) display_sm: f32,
    pub(crate) display: f32,
    pub(crate) display_lg: f32,
    pub(crate) weight_normal: u16,
    pub(crate) weight_medium: u16,
    pub(crate) weight_semibold: u16,
    pub(crate) weight_bold: u16,
    pub(crate) weight_heavy: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Motion {
    pub(crate) fast_ms: u16,
    pub(crate) hover_in_ms: u16,
    pub(crate) hover_out_ms: u16,
    pub(crate) modal_enter_ms: u16,
    pub(crate) modal_exit_ms: u16,
    pub(crate) context_menu_enter_ms: u16,
    pub(crate) context_menu_exit_ms: u16,
    pub(crate) thumb_reveal_ms: u16,
    pub(crate) overlay_toast_ms: u16,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dimensions {
    pub(crate) control_height_sm: f32,
    pub(crate) control_height: f32,
    pub(crate) control_height_lg: f32,
    pub(crate) control_height_xl: f32,
    pub(crate) icon_size_sm: f32,
    pub(crate) icon_size: f32,
    pub(crate) icon_size_md: f32,
    pub(crate) sidebar_band_height: f32,
    pub(crate) sidebar_band_padding_x: f32,
    pub(crate) sidebar_rail_width: f32,
    pub(crate) sidebar_rail_width_inset: f32,
    pub(crate) sidebar_float_margin: f32,
    pub(crate) sidebar_float_radius: f32,
    pub(crate) sidebar_divider_width: f32,
    pub(crate) sidebar_rail_icon_button_size: f32,
    pub(crate) sidebar_rail_icon_glyph: f32,
    pub(crate) sidebar_route_spacing: f32,
    pub(crate) sidebar_account_row_height: f32,
    pub(crate) sidebar_account_rail_avatar_size: f32,
    pub(crate) sidebar_account_rail_box_size: f32,
    pub(crate) presence_badge_size: f32,
    pub(crate) presence_badge_ring: f32,
    pub(crate) account_menu_width: f32,
    pub(crate) account_menu_margin: f32,
    pub(crate) account_menu_bottom_gap: f32,
    pub(crate) account_menu_padding_x: f32,
    pub(crate) account_menu_padding_y: f32,
    pub(crate) account_menu_row_padding_x: f32,
    pub(crate) account_menu_row_padding_y: f32,
    pub(crate) account_menu_update_padding_y: f32,
    pub(crate) account_menu_icon_column_width: f32,
    pub(crate) account_menu_divider_inset: f32,
    pub(crate) account_menu_footer_gap: f32,
    pub(crate) search_palette_top_offset: f32,
    pub(crate) search_palette_width_ratio: f32,
    pub(crate) search_palette_min_width: f32,
    pub(crate) search_palette_max_width: f32,
    pub(crate) search_palette_margin: f32,
    pub(crate) search_keycap_height: f32,
    pub(crate) search_keycap_padding_x: f32,
    pub(crate) search_palette_input_right_padding: f32,
    pub(crate) task_row_height: f32,
    pub(crate) card_padding: f32,
    pub(crate) card_inner_gap: f32,
    pub(crate) card_row_gap: f32,
    pub(crate) card_stats_height: f32,
    pub(crate) card_title_height: f32,
    pub(crate) plus_glyph_size: f32,
    pub(crate) star_rating_width: f32,
    pub(crate) star_rating_height: f32,
    pub(crate) context_menu_row_height: f32,
    pub(crate) context_menu_padding_x: f32,
    pub(crate) context_menu_icon_gap: f32,
    pub(crate) checkbox_size: f32,
    pub(crate) checkbox_icon_size: f32,
    pub(crate) switch_width: f32,
    pub(crate) switch_height: f32,
    pub(crate) switch_knob: f32,
    pub(crate) switch_radius: f32,
    pub(crate) avatar_size: f32,
    pub(crate) tag_height: f32,
    pub(crate) modal_viewport_ratio: f32,
    pub(crate) settings_modal_width: f32,
    pub(crate) settings_modal_height: f32,
    pub(crate) settings_modal_max_width: f32,
    pub(crate) settings_modal_max_height: f32,
    pub(crate) destination_modal_width: f32,
    pub(crate) destination_modal_max_width: f32,
    pub(crate) destination_tile: f32,
    pub(crate) destination_tile_icon: f32,
    pub(crate) destination_tile_icon_gap: f32,
    pub(crate) destination_row_padding: f32,
    pub(crate) destination_history_max_height: f32,
    pub(crate) icon_button_size: f32,
    pub(crate) publish_modal_width: f32,
    pub(crate) publish_modal_max_width: f32,
    pub(crate) publish_modal_max_height: f32,
    pub(crate) preview_modal_width: f32,
    pub(crate) preview_modal_height: f32,
    pub(crate) preview_modal_max_width: f32,
    pub(crate) preview_modal_max_height: f32,
    pub(crate) file_preview_modal_width: f32,
    pub(crate) file_preview_modal_height: f32,
    pub(crate) publish_left_column_width: f32,
    pub(crate) publish_right_column_width: f32,
    pub(crate) publish_icon_preview_height: f32,
    pub(crate) publish_changelog_height: f32,
    pub(crate) browser_empty_icon_size: f32,
    pub(crate) dropdown_item_height: f32,
    pub(crate) popup_max_height: f32,
    pub(crate) textarea_min_height: f32,
    pub(crate) textarea_pref_height: f32,
    pub(crate) border_width: f32,
    pub(crate) focus_border_width: f32,
    pub(crate) scrollbar_thumb_width: f32,
    pub(crate) scrollbar_track_inset: f32,
    pub(crate) disabled_opacity: f32,
    pub(crate) disabled_opacity_strong: f32,
    pub(crate) muted_opacity: f32,
    pub(crate) icon_rest_opacity: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    pub(crate) variant: ThemeVariant,
    pub(crate) colors: Colors,
    pub(crate) spacing: Spacing,
    pub(crate) radii: Radii,
    pub(crate) typography: Typography,
    pub(crate) motion: Motion,
    pub(crate) dims: Dimensions,
}

/// The subset of [`Tokens`] that never varies across theme variants:
/// spacing, radii, typography, motion, and dimensions all come from plain
/// `const fn`s regardless of variant, unlike `colors`. Call sites that only
/// ever needed those fields can read the shared static instead of building
/// a throwaway [`Tokens::dark()`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct InvariantTokens {
    pub(crate) spacing: Spacing,
    pub(crate) radii: Radii,
    pub(crate) typography: Typography,
    pub(crate) motion: Motion,
    pub(crate) dims: Dimensions,
}

static INVARIANT_TOKENS: InvariantTokens = InvariantTokens {
    spacing: spacing(),
    radii: radii(),
    typography: typography(),
    motion: motion(),
    dims: dimensions(),
};

pub(crate) fn invariant() -> &'static InvariantTokens {
    &INVARIANT_TOKENS
}

impl Tokens {
    pub fn dark() -> Self {
        Self::for_variant(ThemeVariant::Dark)
    }

    pub fn light() -> Self {
        Self::for_variant(ThemeVariant::Light)
    }

    pub fn classic_source() -> Self {
        Self::for_variant(ThemeVariant::ClassicSource)
    }

    pub fn for_variant(variant: ThemeVariant) -> Self {
        Self::with_accent_inputs(variant, AccentInputs::for_preset(variant.preset()))
    }

    pub(crate) fn from_effective(
        preset: EffectiveThemePreset,
        accent_inputs: AccentInputs,
    ) -> Self {
        Self::with_accent_inputs(preset.into(), accent_inputs)
    }

    pub fn with_accent_inputs(variant: ThemeVariant, accent_inputs: AccentInputs) -> Self {
        let mut colors = palette_colors(variant);
        let neutral = core_theme::derive(accent_inputs.neutral);
        colors.neutral = neutral.base.into();
        colors.neutral_dark = neutral.dark.into();
        let success = core_theme::derive(accent_inputs.success);
        colors.success = success.base.into();
        colors.success_dark = success.dark.into();
        let error = core_theme::derive(accent_inputs.error);
        colors.error = error.base.into();
        colors.error_dark = error.dark.into();

        Self {
            variant,
            colors,
            spacing: spacing(),
            radii: radii(),
            typography: typography(),
            motion: motion(),
            dims: dimensions(),
        }
    }

    /// Natural content height of the Prepare Publish modal, per mode.
    ///
    /// Upstream sizes the modal `height: min-content`, letting the left
    /// column drive it. iced cannot shrink-wrap a container whose other
    /// columns fill, so the height is computed from the left column instead:
    ///
    ///   padding.top
    ///   + (update mode: workshop-link line + gap)
    ///   + icon well + gap
    ///   + browse-button row + gap
    ///   + 3-line instructions + gap
    ///   + upscale-checkbox row + gap
    ///   + path field + gap
    ///   + title field + gap
    ///   + addon-type select + gap
    ///   + tag selects + gap
    ///   + submit button
    ///   + padding.bottom
    ///
    /// Fields/selects are `pad_control`-padded caption text and buttons are
    /// `pad_control`-padded body text at iced's default 1.3 line height.
    pub(crate) fn publish_modal_height(&self, update_mode: bool) -> f32 {
        const LINE_HEIGHT: f32 = 1.3;
        let caption_line = self.typography.caption * LINE_HEIGHT;
        let body_line = self.typography.body * LINE_HEIGHT;
        let field = self.spacing.pad_control * 2.0 + caption_line;
        let button = self.spacing.pad_control * 2.0 + body_line;

        let blocks = [
            self.dims.publish_icon_preview_height,
            button,              // browse / remove-icon row
            3.0 * body_line,     // icon instructions
            body_line.max(16.0), // upscale checkbox row (16px box)
            field,               // addon path
            field,               // title
            field,               // addon type
            field,               // tag selects
            button,              // submit
        ];
        let mut height = self.spacing.pad * 2.0
            + blocks.iter().sum::<f32>()
            + (blocks.len() - 1) as f32 * self.spacing.gap;
        if update_mode {
            height += body_line + self.spacing.gap; // workshop-page link row
        }
        height
    }

    pub fn iced_theme(self) -> Theme {
        Theme::custom(
            self.variant.name(),
            Palette {
                background: self.colors.bg.into(),
                text: self.colors.text.into(),
                primary: self.colors.neutral.into(),
                success: self.colors.success.into(),
                warning: self.colors.link.into(),
                danger: self.colors.error.into(),
            },
        )
    }
}

/// Semantic Steam Workshop tag chip palette shared across themes. Returns
/// (background, text); unknown tags get the base white chip with black text.
pub(crate) fn workshop_tag_colors(tag: &str) -> (Rgba, Rgba) {
    let white = rgb(0xFFFFFF);
    let black = rgb(0x000000);
    match tag.to_ascii_lowercase().as_str() {
        "addon" => (rgb(0x006CC7), white),
        "weapon" => (rgb(0x8C0101), white),
        "servercontent" => (rgb(0x000000), white),
        "fun" => (rgb(0x368C01), white),
        "roleplay" => (rgb(0x00D4D4), black),
        "realism" => (rgb(0x8400D6), white),
        "vehicle" => (rgb(0x5D3131), white),
        "movie" => (rgb(0x47AB94), white),
        "cartoon" | "comic" => (rgb(0x642865), white),
        "scenic" => (rgb(0xFB9E9E), black),
        "water" => (rgb(0x4754AB), white),
        "build" => (rgb(0x3E6E79), white),
        "tool" => (rgb(0xB98528), white),
        "gamemode" => (rgb(0x88CC86), black),
        "map" => (rgb(0x804100), white),
        "npc" => (rgb(0xFDFA8E), black),
        "effects" => (rgb(0x27C500), black),
        "model" => (rgb(0x80007C), white),
        _ => (white, black),
    }
}

fn palette_colors(variant: ThemeVariant) -> Colors {
    match variant {
        ThemeVariant::Dark => dark_colors(),
        ThemeVariant::Light => light_colors(),
        ThemeVariant::ClassicSource => classic_source_colors(),
    }
}

const fn alpha(percent: u8) -> u8 {
    (((percent as u16) * 255 + 50) / 100) as u8
}

const fn rgb(value: u32) -> Rgba {
    Rgba::rgb(value)
}

const fn rgba(value: u32, percent: u8) -> Rgba {
    Rgba::from_rgba(value, alpha(percent))
}

fn dark_colors() -> Colors {
    Colors {
        neutral: rgb(0x006DC7),
        neutral_dark: rgb(0x005DA9),
        success: rgb(0x30A661),
        success_dark: rgb(0x247D4A),
        star_rating_filled: rgb(0x6BB64D),
        star_rating_empty: rgb(0x424242),
        download_count_icon: rgb(0x6BB64D),
        error: rgb(0xA80000),
        error_dark: rgb(0x7E0000),
        link: rgb(0x46B0FF),
        text: rgb(0xFFFFFF),
        text_dim: rgb(0x888888),
        // Opaque sRGB composite of 50% white over the dark slabs; iced's
        // gamma-correct text blending renders alpha text far brighter than
        // the CSS reference, so the mix is baked in.
        text_watermark: rgb(0x949494),
        text_inverted: rgb(0x000000),
        text_on_neutral: rgb(0xFFFFFF),
        text_on_success: rgb(0xFFFFFF),
        text_on_error: rgb(0xFFFFFF),
        bg: rgb(0x1A1A1A),
        sidebar_panel_bg: rgb(0x232323),
        surface: rgb(0x232323),
        surface_2: rgb(0x2A2A2A),
        surface_raised: rgb(0x292929),
        surface_muted: rgb(0x212121),
        surface_sunken: rgb(0x101010),
        surface_deep: rgb(0x0E0E0E),
        surface_preview: rgb(0x0C0C0C),
        surface_preview_card: rgb(0x171717),
        chrome_deep: rgb(0x0A0A0A),
        sidebar_item_hover: rgb(0x2D2D2D),
        // Alpha fills over known surfaces are pre-composited in sRGB space
        // (as CSS does); iced blends alpha quads in linear space, which
        // renders white-alpha fills visibly lighter than the reference.
        // input_bg = white@10% over modal_bg #1A1A1A
        input_bg: rgb(0x313131),
        // search_input_bg = white@5% over overlay_panel_bg #232323
        search_input_bg: rgb(0x2E2E2E),
        search_scrim: rgba(0x000000, 30),
        overlay_panel_bg: rgb(0x232323),
        // overlay_divider = white@5% over overlay_panel_bg #232323
        overlay_divider: rgb(0x2E2E2E),
        // search_keycap_border = white@9% over overlay_panel_bg #232323
        search_keycap_border: rgb(0x3A3A3A),
        // account_update_bg = link@18% over overlay_panel_bg #232323
        account_update_bg: rgb(0x293C4B),
        // account_update_hover_bg = link@24% over overlay_panel_bg #232323
        account_update_hover_bg: rgb(0x2B4558),
        destination_input_bg: rgb(0x0E0E0E),
        // brightness(.5) dim baked as opaque halves: tile bg #292929 ->
        // #151515, white text -> #808080.
        tile_disabled_bg: rgb(0x151515),
        tile_disabled_text: rgb(0x808080),
        // row_stripe = black@12% over surface_raised #292929
        row_stripe: rgb(0x242424),
        // browser_empty_dim = white@25% over surface_raised #292929
        browser_empty_dim: rgb(0x5F5F5F),
        tooltip_bg: rgb(0x0F0F0F),
        menu_bg: rgb(0x4A4A4A),
        dropdown_bg: rgb(0x474747),
        modal_bg: rgb(0x1A1A1A),
        preview_modal_bg: rgb(0x131313),
        extract_disabled_bg: rgb(0x3B3B3B),
        // browser_shortcut_dim = text@50% over preview_modal_bg #131313
        browser_shortcut_dim: rgb(0x898989),
        menu_option_selected_bg: rgb(0xCECECE),
        menu_option_selected_text: rgb(0x313131),
        button_bg: rgb(0x313131),
        button_pressed: rgb(0x252525),
        control_bg: rgb(0x313131),
        control_bg_alt: rgb(0x212121),
        control_pressed: rgb(0x1B1B1B),
        switch_on: rgb(0x009AFF),
        switch_off: rgb(0x949494),
        switch_knob: rgb(0xFFFFFF),
        icon_muted: rgb(0x424242),
        border: rgb(0x101010),
        border_strong: rgb(0x080808),
        border_subtle: rgba(0xFFFFFF, 18),
        divider: rgb(0x101010),
        divider_strong: rgb(0x131313),
        checkbox_border: rgb(0x6A6A6A),
        focus_ring: rgb(0x127CFF),
        hover_fill: rgba(0xFFFFFF, 10),
        hover_fill_medium: rgba(0xFFFFFF, 7),
        hover_fill_subtle: rgba(0xFFFFFF, 6),
        hover_fill_soft: rgba(0xFFFFFF, 8),
        hover_fill_faint: rgba(0xFFFFFF, 3),
        row_hover_fill: rgba(0x000000, 16),
        row_hover_fill_strong: rgba(0x000000, 20),
        row_fill: rgba(0x000000, 12),
        row_fill_subtle: rgba(0x000000, 10),
        row_fill_alt: rgba(0x000000, 24),
        row_fill_soft: rgba(0x000000, 8),
        row_fill_medium: rgba(0x000000, 18),
        selected_fill: rgb(0x2A2A2A),
        selected_fill_strong: rgb(0x101010),
        scrollbar_grabber: rgb(0x2D2D2D),
        scrollbar_grabber_hover: rgb(0x3A3A3A),
        scrollbar_grabber_active: rgb(0x444444),
        scrollbar_rail: rgb(0x232323),
        scrim: rgba(0x000000, 40),
        scrim_strong: rgba(0x000000, 48),
        scrim_soft: rgba(0x000000, 30),
        scrim_expanded: rgba(0x000000, 200),
        overlay_fill: rgba(0x000000, 36),
        overlay_fill_soft: rgba(0x000000, 18),
        shadow: rgba(0x000000, 30),
        shadow_soft: rgba(0x000000, 25),
        shadow_control: rgba(0x000000, 40),
        shadow_action: rgba(0x000000, 10),
        shadow_raised: rgba(0x000000, 35),
        shadow_card: rgba(0x000000, 40),
        shadow_strong: rgba(0x000000, 42),
        shadow_card_strong: rgba(0x000000, 50),
        shadow_dropdown: rgba(0x000000, 62),
    }
}

fn light_colors() -> Colors {
    Colors {
        neutral: rgb(0x006DC7),
        neutral_dark: rgb(0x005DA9),
        success: rgb(0x258F52),
        success_dark: rgb(0x1F7945),
        star_rating_filled: rgb(0x258F52),
        star_rating_empty: rgb(0xC6D0DA),
        download_count_icon: rgb(0x258F52),
        error: rgb(0xB3261E),
        error_dark: rgb(0x982019),
        link: rgb(0x006DC7),
        text: rgb(0x1D232A),
        text_dim: rgb(0x626E7A),
        text_watermark: rgb(0x8E9194),
        text_inverted: rgb(0x000000),
        text_on_neutral: rgb(0xFFFFFF),
        text_on_success: rgb(0xFFFFFF),
        text_on_error: rgb(0xFFFFFF),
        bg: rgb(0xF3F5F7),
        sidebar_panel_bg: rgb(0xFFFFFF),
        surface: rgb(0xFFFFFF),
        surface_2: rgb(0xE8EDF2),
        surface_raised: rgb(0xFFFFFF),
        surface_muted: rgb(0xEEF2F5),
        surface_sunken: rgb(0xDCE3EA),
        surface_deep: rgb(0xF8FAFC),
        surface_preview: rgb(0xEEF2F5),
        surface_preview_card: rgb(0xFFFFFF),
        chrome_deep: rgb(0xE1E7ED),
        sidebar_item_hover: rgb(0xDDE5EC),
        // Pre-composited sRGB blends over the light surfaces (see dark).
        // input_bg = black@15% over modal_bg #FFFFFF
        input_bg: rgb(0xD9D9D9),
        // search_input_bg = black@5% over overlay_panel_bg #FFFFFF
        search_input_bg: rgb(0xF2F2F2),
        search_scrim: rgba(0x000000, 30),
        overlay_panel_bg: rgb(0xFFFFFF),
        // overlay_divider = black@9% over overlay_panel_bg #FFFFFF
        overlay_divider: rgb(0xE8E8E8),
        // search_keycap_border = black@15% over overlay_panel_bg #FFFFFF
        search_keycap_border: rgb(0xD9D9D9),
        // account_update_bg = link@10% over overlay_panel_bg #FFFFFF
        account_update_bg: rgb(0xE6F0F9),
        // account_update_hover_bg = link@14% over overlay_panel_bg #FFFFFF
        account_update_hover_bg: rgb(0xDBEBF7),
        // Destination Select path well: sunken light-grey counterpart.
        destination_input_bg: rgb(0xE3E7EB),
        // Disabled tiles fade toward the page bg with watermark-grade text
        // (the dark theme's brightness(.5) rule has no light equivalent).
        tile_disabled_bg: rgb(0xF1F3F5),
        tile_disabled_text: rgb(0x8E9194),
        // row_stripe = black@12% over surface_raised #FFFFFF
        row_stripe: rgb(0xE0E0E0),
        // browser_empty_dim = text@25% over surface_raised #FFFFFF
        browser_empty_dim: rgb(0xC7C8CA),
        tooltip_bg: rgb(0x242A30),
        menu_bg: rgb(0xFFFFFF),
        dropdown_bg: rgb(0xFFFFFF),
        modal_bg: rgb(0xFFFFFF),
        preview_modal_bg: rgb(0xF0F3F6),
        extract_disabled_bg: rgb(0xB9C0C7),
        // browser_shortcut_dim = text@50% over preview_modal_bg #F0F3F6
        browser_shortcut_dim: rgb(0x878B90),
        menu_option_selected_bg: rgb(0x3A424A),
        menu_option_selected_text: rgb(0xEEF2F6),
        button_bg: rgb(0xEEF2F6),
        button_pressed: rgb(0xD9E1E8),
        control_bg: rgb(0xEEF2F6),
        control_bg_alt: rgb(0xE6EBF0),
        control_pressed: rgb(0xCBD6DF),
        switch_on: rgb(0x0B73D9),
        switch_off: rgb(0xA7B0B9),
        switch_knob: rgb(0xFFFFFF),
        icon_muted: rgb(0x7C8793),
        border: rgb(0xC6D0DA),
        border_strong: rgb(0xAAB7C4),
        border_subtle: rgba(0x000000, 18),
        divider: rgb(0xC6D0DA),
        divider_strong: rgb(0xAEB9C5),
        checkbox_border: rgb(0x737B84),
        focus_ring: rgb(0x0B73D9),
        hover_fill: rgba(0x000000, 20),
        hover_fill_medium: rgba(0x000000, 7),
        hover_fill_subtle: rgba(0x000000, 6),
        hover_fill_soft: rgba(0x000000, 8),
        hover_fill_faint: rgba(0x000000, 3),
        row_hover_fill: rgba(0x000000, 16),
        row_hover_fill_strong: rgba(0x000000, 20),
        row_fill: rgba(0x000000, 12),
        row_fill_subtle: rgba(0x000000, 10),
        row_fill_alt: rgba(0x000000, 24),
        row_fill_soft: rgba(0x000000, 8),
        row_fill_medium: rgba(0x000000, 18),
        selected_fill: rgb(0xE8EDF2),
        selected_fill_strong: rgb(0xDCE3EA),
        scrollbar_grabber: rgb(0xDDE5EC),
        scrollbar_grabber_hover: rgb(0xC6D0DA),
        scrollbar_grabber_active: rgb(0xAEB9C5),
        scrollbar_rail: rgb(0xFFFFFF),
        scrim: rgba(0x000000, 40),
        scrim_strong: rgba(0x000000, 48),
        scrim_soft: rgba(0x000000, 30),
        scrim_expanded: rgba(0x000000, 200),
        overlay_fill: rgba(0x000000, 36),
        overlay_fill_soft: rgba(0x000000, 18),
        shadow: rgba(0x000000, 30),
        shadow_soft: rgba(0x000000, 25),
        shadow_control: rgba(0x000000, 24),
        shadow_action: rgba(0x000000, 10),
        shadow_raised: rgba(0x000000, 35),
        shadow_card: rgba(0x000000, 56),
        shadow_strong: rgba(0x000000, 42),
        shadow_card_strong: rgba(0x000000, 50),
        shadow_dropdown: rgba(0x000000, 92),
    }
}

fn classic_source_colors() -> Colors {
    Colors {
        neutral: rgb(0xE08A2E),
        neutral_dark: rgb(0xC8761E),
        success: rgb(0x879A57),
        success_dark: rgb(0x73834A),
        star_rating_filled: rgb(0x879A57),
        star_rating_empty: rgb(0x5B6150),
        download_count_icon: rgb(0x879A57),
        error: rgb(0xB85E42),
        error_dark: rgb(0x9C5038),
        link: rgb(0xE08A2E),
        text: rgb(0xF2ECD8),
        text_dim: rgb(0xBBB696),
        text_watermark: rgb(0x8A8A7A),
        text_inverted: rgb(0x000000),
        text_on_neutral: rgb(0x141811),
        text_on_success: rgb(0x141811),
        text_on_error: rgb(0xFFFFFF),
        bg: rgb(0x141811),
        sidebar_panel_bg: rgb(0x22291C),
        surface: rgb(0x22291C),
        surface_2: rgb(0x333E29),
        surface_raised: rgb(0x293223),
        surface_muted: rgb(0x1B2118),
        surface_sunken: rgb(0x0F130D),
        surface_deep: rgb(0x0A0D08),
        surface_preview: rgb(0x0E120D),
        surface_preview_card: rgb(0x1B2117),
        chrome_deep: rgb(0x0F120D),
        sidebar_item_hover: rgb(0x3A3121),
        // Pre-composited sRGB blends over the classic surfaces (see dark).
        // input_bg = #F2ECD8@9% over modal_bg #141811
        input_bg: rgb(0x282B23),
        // search_input_bg = #F2ECD8@5% over overlay_panel_bg #22291C
        search_input_bg: rgb(0x2D3325),
        search_scrim: rgba(0x000000, 30),
        overlay_panel_bg: rgb(0x22291C),
        // overlay_divider = #F2ECD8@5% over overlay_panel_bg #22291C
        overlay_divider: rgb(0x2D3325),
        // search_keycap_border = #F2ECD8@10% over overlay_panel_bg #22291C
        search_keycap_border: rgb(0x373C2F),
        // account_update_bg = link@16% over overlay_panel_bg #22291C
        account_update_bg: rgb(0x40381F),
        // account_update_hover_bg = link@22% over overlay_panel_bg #22291C
        account_update_hover_bg: rgb(0x4C3E21),
        // Destination Select path well: near-black green-tinted counterpart.
        destination_input_bg: rgb(0x0B0E09),
        // brightness(.5) of the classic tile surfaces, baked opaque:
        // tile bg #293223 -> #141911, text #F2ECD8 -> #79766C.
        tile_disabled_bg: rgb(0x141911),
        tile_disabled_text: rgb(0x79766C),
        // row_stripe = black@12% over surface_raised #293223
        row_stripe: rgb(0x242C1F),
        // browser_empty_dim = text@25% over surface_raised #293223
        browser_empty_dim: rgb(0x5B6150),
        tooltip_bg: rgb(0x0D100B),
        menu_bg: rgb(0x394230),
        dropdown_bg: rgb(0x343D2B),
        modal_bg: rgb(0x141811),
        preview_modal_bg: rgb(0x10140E),
        extract_disabled_bg: rgb(0x3F4633),
        // browser_shortcut_dim = text@50% over preview_modal_bg #10140E
        browser_shortcut_dim: rgb(0x818073),
        menu_option_selected_bg: rgb(0xD9D2B9),
        menu_option_selected_text: rgb(0x22291C),
        button_bg: rgb(0x343B2A),
        button_pressed: rgb(0x272D20),
        control_bg: rgb(0x343B2A),
        control_bg_alt: rgb(0x1B2118),
        control_pressed: rgb(0x181D14),
        switch_on: rgb(0xE08A2E),
        switch_off: rgb(0x777C68),
        switch_knob: rgb(0xF2ECD8),
        icon_muted: rgb(0xA8A886),
        border: rgb(0x0A0D08),
        border_strong: rgb(0x050704),
        border_subtle: rgba(0xF2ECD8, 22),
        divider: rgb(0x0D100B),
        divider_strong: rgb(0x151A12),
        checkbox_border: rgb(0x9A9B7B),
        focus_ring: rgb(0xE08A2E),
        hover_fill: rgba(0xE08A2E, 12),
        hover_fill_medium: rgba(0xE08A2E, 9),
        hover_fill_subtle: rgba(0xE08A2E, 7),
        hover_fill_soft: rgba(0xE08A2E, 9),
        hover_fill_faint: rgba(0xE08A2E, 5),
        row_hover_fill: rgba(0x000000, 16),
        row_hover_fill_strong: rgba(0x000000, 20),
        row_fill: rgba(0x000000, 12),
        row_fill_subtle: rgba(0x000000, 10),
        row_fill_alt: rgba(0x000000, 24),
        row_fill_soft: rgba(0x000000, 8),
        row_fill_medium: rgba(0x000000, 18),
        selected_fill: rgb(0x333E29),
        selected_fill_strong: rgb(0x0F130D),
        scrollbar_grabber: rgb(0x3A3121),
        scrollbar_grabber_hover: rgb(0x4C3E21),
        scrollbar_grabber_active: rgb(0x5B4824),
        scrollbar_rail: rgb(0x22291C),
        scrim: rgba(0x000000, 40),
        scrim_strong: rgba(0x000000, 48),
        scrim_soft: rgba(0x000000, 30),
        scrim_expanded: rgba(0x000000, 200),
        overlay_fill: rgba(0x000000, 36),
        overlay_fill_soft: rgba(0x000000, 18),
        shadow: rgba(0x000000, 30),
        shadow_soft: rgba(0x000000, 25),
        shadow_control: rgba(0x000000, 40),
        shadow_action: rgba(0x000000, 10),
        shadow_raised: rgba(0x000000, 35),
        shadow_card: rgba(0x000000, 40),
        shadow_strong: rgba(0x000000, 42),
        shadow_card_strong: rgba(0x000000, 50),
        shadow_dropdown: rgba(0x000000, 62),
    }
}

const fn spacing() -> Spacing {
    Spacing {
        gap_xs: 4.0,
        gap_sm: 8.0,
        gap_md: 12.0,
        gap: 16.0,
        gap_lg: 24.0,
        pad_xs: 4.0,
        pad_sm: 12.0,
        pad_control: 11.0,
        pad_control_x: 14.0,
        pad_control_y: 8.0,
        pad: 24.0,
    }
}

const fn radii() -> Radii {
    Radii {
        xs: 2.0,
        base: 4.0,
        md: 12.0,
        lg: 6.0,
    }
}

const fn typography() -> Typography {
    Typography {
        caption_xs: 11.0,
        caption: 12.0,
        body_sm: 13.0,
        body: 14.0,
        body_lg: 15.0,
        title_xs: 16.0,
        title_sm: 17.0,
        title: 18.0,
        title_lg: 20.0,
        display_xs: 21.0,
        display_sm: 22.0,
        display: 24.0,
        display_lg: 26.0,
        weight_normal: 400,
        weight_medium: 500,
        weight_semibold: 600,
        weight_bold: 700,
        weight_heavy: 800,
    }
}

const fn motion() -> Motion {
    Motion {
        fast_ms: 100,
        hover_in_ms: 90,
        hover_out_ms: 220,
        modal_enter_ms: 180,
        modal_exit_ms: 130,
        context_menu_enter_ms: 120,
        context_menu_exit_ms: 100,
        thumb_reveal_ms: 150,
        overlay_toast_ms: 500,
    }
}

const fn dimensions() -> Dimensions {
    Dimensions {
        control_height_sm: 32.0,
        control_height: 40.0,
        control_height_lg: 44.0,
        control_height_xl: 48.0,
        icon_size_sm: 12.0,
        icon_size: 16.0,
        icon_size_md: 18.0,
        sidebar_band_height: 38.0,
        sidebar_band_padding_x: 12.0,
        sidebar_rail_width: 52.0,
        sidebar_rail_width_inset: 84.0,
        sidebar_float_margin: 10.0,
        sidebar_float_radius: 12.0,
        sidebar_divider_width: 1.0,
        sidebar_rail_icon_button_size: 36.0,
        sidebar_rail_icon_glyph: 20.0,
        sidebar_route_spacing: 8.0,
        sidebar_account_row_height: 56.0,
        sidebar_account_rail_avatar_size: 40.0,
        sidebar_account_rail_box_size: 48.0,
        presence_badge_size: 9.0,
        presence_badge_ring: 2.0,
        account_menu_width: 248.0,
        account_menu_margin: 12.0,
        account_menu_bottom_gap: 8.0,
        account_menu_padding_x: 6.0,
        account_menu_padding_y: 6.0,
        account_menu_row_padding_x: 6.0,
        account_menu_row_padding_y: 8.0,
        account_menu_update_padding_y: 8.0,
        account_menu_icon_column_width: 22.0,
        account_menu_divider_inset: 8.0,
        account_menu_footer_gap: 1.0,
        search_palette_top_offset: 72.0,
        search_palette_width_ratio: 0.6,
        search_palette_min_width: 420.0,
        search_palette_max_width: 560.0,
        search_palette_margin: 16.0,
        search_keycap_height: 22.0,
        search_keycap_padding_x: 6.0,
        search_palette_input_right_padding: 60.0,
        task_row_height: 49.0,
        card_padding: 13.0,
        card_inner_gap: 13.0,
        card_row_gap: 10.0,
        card_stats_height: 18.0,
        card_title_height: 54.0,
        plus_glyph_size: 64.0,
        star_rating_width: 81.0,
        star_rating_height: 14.0,
        context_menu_row_height: 38.0,
        context_menu_padding_x: 11.0,
        context_menu_icon_gap: 8.0,
        checkbox_size: 14.0,
        checkbox_icon_size: 10.0,
        switch_width: 40.0,
        switch_height: 20.0,
        switch_knob: 16.0,
        switch_radius: 8.0,
        avatar_size: 44.0,
        tag_height: 18.0,
        modal_viewport_ratio: 0.9,
        settings_modal_width: 672.0,
        settings_modal_height: 480.0,
        settings_modal_max_width: 960.0,
        settings_modal_max_height: 720.0,
        // Destination chooser: 4 x 7rem tiles + 3 x 1rem gaps + 1.5rem
        // padding per side = 34rem (544px); tiles 7rem/2.5rem icon/.6rem
        // icon gap, .6rem history-row padding, ~12rem history cap.
        destination_modal_width: 544.0,
        destination_modal_max_width: 672.0,
        destination_tile: 112.0,
        destination_tile_icon: 40.0,
        destination_tile_icon_gap: 10.0,
        destination_row_padding: 10.0,
        destination_history_max_height: 192.0,
        icon_button_size: 38.0,
        publish_modal_width: 1168.0,
        publish_modal_max_width: 1600.0,
        publish_modal_max_height: 1000.0,
        preview_modal_width: 1008.0,
        preview_modal_height: 704.0,
        preview_modal_max_width: 1600.0,
        preview_modal_max_height: 1100.0,
        file_preview_modal_width: 832.0,
        file_preview_modal_height: 640.0,
        publish_left_column_width: 288.0,
        publish_right_column_width: 224.0,
        publish_icon_preview_height: 240.0,
        publish_changelog_height: 198.0,
        browser_empty_icon_size: 64.0,
        dropdown_item_height: 36.0,
        popup_max_height: 240.0,
        textarea_min_height: 96.0,
        textarea_pref_height: 160.0,
        border_width: 1.0,
        focus_border_width: 1.5,
        scrollbar_thumb_width: 6.0,
        scrollbar_track_inset: 5.0,
        disabled_opacity: 0.5,
        disabled_opacity_strong: 0.45,
        muted_opacity: 0.35,
        icon_rest_opacity: 0.30,
    }
}

#[cfg(test)]
mod tests {
    use crate::bridge::{SystemColorScheme, ThemePreset, effective_theme_preset};

    use super::{AccentInputs, Rgba, ThemeVariant, Tokens, alpha};

    #[test]
    fn dark_tokens_match_spec_table_values() {
        let tokens = Tokens::dark();

        assert_eq!(tokens.colors.bg, Rgba::rgb(0x1A1A1A));
        assert_eq!(tokens.colors.sidebar_panel_bg, Rgba::rgb(0x232323));
        assert_eq!(tokens.colors.surface_preview_card, Rgba::rgb(0x171717));
        assert_eq!(tokens.colors.link, Rgba::rgb(0x46B0FF));
        assert_eq!(tokens.colors.star_rating_filled, Rgba::rgb(0x6BB64D));
        assert_eq!(tokens.colors.star_rating_empty, Rgba::rgb(0x424242));
        assert_eq!(tokens.colors.download_count_icon, Rgba::rgb(0x6BB64D));
        // white@10% over #1A1A1A, composited in sRGB space
        assert_eq!(tokens.colors.input_bg, Rgba::rgb(0x313131));
        assert_eq!(tokens.colors.text_watermark, Rgba::rgb(0x949494));
        assert_eq!(tokens.colors.overlay_panel_bg, Rgba::rgb(0x232323));
        assert_eq!(tokens.colors.overlay_divider, Rgba::rgb(0x2E2E2E));
        assert_eq!(tokens.colors.search_keycap_border, Rgba::rgb(0x3A3A3A));
        // white@5% over overlay_panel_bg #232323
        assert_eq!(tokens.colors.search_input_bg, Rgba::rgb(0x2E2E2E));
        assert_eq!(
            tokens.colors.search_scrim,
            Rgba::from_rgba(0x000000, alpha(30))
        );
        assert_eq!(tokens.colors.account_update_bg, Rgba::rgb(0x293C4B));
        assert_eq!(tokens.colors.account_update_hover_bg, Rgba::rgb(0x2B4558));
        // black@12% over the #292929 sunken card
        assert_eq!(tokens.colors.row_stripe, Rgba::rgb(0x242424));
        assert_eq!(tokens.colors.destination_input_bg, Rgba::rgb(0x0E0E0E));
        assert_eq!(tokens.colors.tile_disabled_bg, Rgba::rgb(0x151515));
        assert_eq!(tokens.colors.tile_disabled_text, Rgba::rgb(0x808080));
        assert_eq!(tokens.dims.destination_modal_width, 544.0);
        assert_eq!(tokens.dims.destination_tile, 112.0);
        // white@25% over the #292929 sunken card
        assert_eq!(tokens.colors.browser_empty_dim, Rgba::rgb(0x5F5F5F));
        assert_eq!(tokens.colors.preview_modal_bg, Rgba::rgb(0x131313));
        assert_eq!(tokens.colors.extract_disabled_bg, Rgba::rgb(0x3B3B3B));
        // text@50% over the #131313 preview surface
        assert_eq!(tokens.colors.browser_shortcut_dim, Rgba::rgb(0x898989));
        // shared workshop tag palette is theme-independent
        assert_eq!(
            super::workshop_tag_colors("Map"),
            (Rgba::rgb(0x804100), Rgba::rgb(0xFFFFFF))
        );
        assert_eq!(
            super::workshop_tag_colors("npc"),
            (Rgba::rgb(0xFDFA8E), Rgba::rgb(0x000000))
        );
        assert_eq!(
            super::workshop_tag_colors("mystery"),
            (Rgba::rgb(0xFFFFFF), Rgba::rgb(0x000000))
        );
        assert_eq!(tokens.colors.scrollbar_rail, tokens.colors.sidebar_panel_bg);
        assert_eq!(
            tokens.colors.scrollbar_grabber,
            tokens.colors.sidebar_item_hover
        );
        assert_eq!(tokens.spacing.gap, 16.0);
        assert_eq!(tokens.radii.base, 4.0);
        assert_eq!(tokens.radii.md, 12.0);
        assert_eq!(tokens.typography.body, 14.0);
        assert_eq!(tokens.motion.modal_enter_ms, 180);
        assert_eq!(tokens.dims.sidebar_band_height, 38.0);
        assert_eq!(tokens.dims.sidebar_band_padding_x, 12.0);
        assert_eq!(tokens.dims.sidebar_rail_width, 52.0);
        assert_eq!(tokens.dims.sidebar_rail_width_inset, 84.0);
        assert_eq!(tokens.dims.sidebar_float_margin, 10.0);
        assert_eq!(tokens.dims.sidebar_float_radius, 12.0);
        assert_eq!(tokens.dims.sidebar_rail_icon_button_size, 36.0);
        assert_eq!(tokens.dims.sidebar_rail_icon_glyph, 20.0);
        assert_eq!(tokens.dims.sidebar_account_row_height, 56.0);
        assert_eq!(tokens.dims.sidebar_account_rail_avatar_size, 40.0);
        assert_eq!(tokens.dims.sidebar_account_rail_box_size, 48.0);
        assert_eq!(tokens.dims.account_menu_width, 248.0);
        assert_eq!(tokens.dims.account_menu_padding_x, 6.0);
        assert_eq!(tokens.dims.account_menu_padding_y, 6.0);
        assert_eq!(tokens.dims.account_menu_icon_column_width, 22.0);
        assert_eq!(tokens.dims.account_menu_divider_inset, 8.0);
        assert_eq!(tokens.dims.search_palette_top_offset, 72.0);
        assert_eq!(tokens.dims.search_palette_min_width, 420.0);
        assert_eq!(tokens.dims.search_palette_input_right_padding, 60.0);
        assert_eq!(tokens.dims.card_row_gap, 10.0);
    }

    #[test]
    fn rail_sidebar_scale_tokens_are_theme_independent() {
        for tokens in [Tokens::dark(), Tokens::light(), Tokens::classic_source()] {
            assert_eq!(tokens.dims.sidebar_rail_icon_button_size, 36.0);
            assert_eq!(tokens.dims.sidebar_rail_icon_glyph, 20.0);
            assert_eq!(tokens.dims.sidebar_account_rail_avatar_size, 40.0);
            assert_eq!(tokens.dims.sidebar_account_rail_box_size, 48.0);
        }
    }

    #[test]
    fn publish_modal_height_follows_the_left_column_formula() {
        let tokens = Tokens::dark();

        let field = tokens.spacing.pad_control * 2.0 + tokens.typography.caption * 1.3;
        let button = tokens.spacing.pad_control * 2.0 + tokens.typography.body * 1.3;
        let body_line = tokens.typography.body * 1.3;
        let expected_new = tokens.spacing.pad * 2.0
            + tokens.dims.publish_icon_preview_height
            + 3.0 * body_line
            + body_line.max(16.0)
            + 4.0 * field
            + 2.0 * button
            + 8.0 * tokens.spacing.gap;

        assert!((tokens.publish_modal_height(false) - expected_new).abs() < 0.01);
        assert!(
            (tokens.publish_modal_height(true) - (expected_new + body_line + tokens.spacing.gap))
                .abs()
                < 0.01
        );
    }

    #[test]
    fn all_three_variants_render_distinct_surfaces() {
        let dark = Tokens::dark();
        let light = Tokens::light();
        let classic = Tokens::classic_source();

        assert_ne!(dark.colors.bg, light.colors.bg);
        assert_ne!(dark.colors.bg, classic.colors.bg);
        assert_ne!(light.colors.bg, classic.colors.bg);
        for tokens in [dark, light, classic] {
            assert_eq!(tokens.colors.scrollbar_rail, tokens.colors.sidebar_panel_bg);
            assert_eq!(
                tokens.colors.scrollbar_grabber,
                tokens.colors.sidebar_item_hover
            );
        }
        assert_eq!(classic.colors.link, Rgba::rgb(0xE08A2E));
        assert_eq!(light.colors.star_rating_empty, Rgba::rgb(0xC6D0DA));
        assert_eq!(classic.colors.star_rating_empty, Rgba::rgb(0x5B6150));
        assert_eq!(light.colors.download_count_icon, Rgba::rgb(0x258F52));
        assert_eq!(classic.colors.download_count_icon, Rgba::rgb(0x879A57));
    }

    #[test]
    fn custom_accents_follow_the_backend_hsl_derivation_rule() {
        let tokens = Tokens::with_accent_inputs(
            ThemeVariant::Dark,
            AccentInputs {
                neutral: 0x00E08A2E,
                success: 0x00879A57,
                error: 0x00B85E42,
            },
        );

        assert_eq!(tokens.colors.neutral, Rgba::rgb(0xE08A2E));
        assert_eq!(tokens.colors.neutral_dark, Rgba::rgb(0xC8761E));
        assert_eq!(tokens.colors.success_dark, Rgba::rgb(0x73834A));
        assert_eq!(tokens.colors.error_dark, Rgba::rgb(0x9C5038));
    }

    #[test]
    fn effective_theme_conversion_uses_resolved_variant_only() {
        let effective = effective_theme_preset(ThemePreset::Auto, SystemColorScheme::Light);
        let tokens = Tokens::from_effective(
            effective,
            AccentInputs {
                neutral: 0x00006DC7,
                success: 0x0030A661,
                error: 0x00A80000,
            },
        );

        assert_eq!(tokens.variant, ThemeVariant::Light);
        assert_eq!(tokens.colors.bg, Rgba::rgb(0xF3F5F7));
        assert_eq!(tokens.colors.neutral, Rgba::rgb(0x006DC7));
    }
}
