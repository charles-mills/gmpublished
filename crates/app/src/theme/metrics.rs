use crate::bridge::{MAX_TEXT_SIZE_PERCENT, MIN_TEXT_SIZE_PERCENT, UiDensity};

use super::{LayoutMetrics, Tokens, invariant};

impl Tokens {
    pub(crate) fn with_display_preferences(self, density: UiDensity, text_percent: u16) -> Self {
        self.with_density_progress(
            if density == UiDensity::Compact {
                1.0
            } else {
                0.0
            },
            text_percent,
        )
    }

    pub(crate) fn with_density_progress(mut self, compact: f32, text_percent: u16) -> Self {
        let compact = compact.clamp(0.0, 1.0);
        let between = |comfortable: f32, tight: f32| comfortable + (tight - comfortable) * compact;
        let base = invariant();
        self.typography = base.typography;
        self.spacing = base.spacing;
        self.dims = base.dims;
        self.text_scale_factor =
            text_percent.clamp(MIN_TEXT_SIZE_PERCENT, MAX_TEXT_SIZE_PERCENT) as f32 / 100.0;
        self.spacing_scale_factor = between(1.0, 0.8);
        self.row_scale_factor = self.spacing_scale_factor + (self.text_scale_factor - 1.0).max(0.0);
        self.preferred_card_width = between(280.0, 220.0);

        macro_rules! text_roles {
            ($($field:ident),+ $(,)?) => {$(
                self.typography.$field = self.text_size(base.typography.$field);
            )+};
        }
        text_roles!(
            caption_xs, caption, body_sm, body, body_lg, title_xs, title_sm, title, title_lg,
            display_xs, display_sm, display, display_lg
        );

        macro_rules! spaces {
            ($($field:ident),+ $(,)?) => {$(
                self.spacing.$field = self.space(base.spacing.$field);
            )+};
        }
        spaces!(
            gap_xs,
            gap_sm,
            gap_md,
            gap,
            gap_lg,
            pad_xs,
            pad_sm,
            pad_control,
            pad_control_x,
            pad_control_y,
            pad
        );
        self.spacing.gap = between(24.0, 16.0);
        self.spacing.pad = between(28.0, 20.0);
        self.spacing.pad_control_x = between(14.0, 10.0);

        macro_rules! rows {
            ($($field:ident),+ $(,)?) => {$(
                self.dims.$field = self.row_height(base.dims.$field);
            )+};
        }
        rows!(
            control_height_sm,
            control_height,
            control_height_lg,
            control_height_xl,
            sidebar_rail_icon_button_size,
            sidebar_rail_icon_button_size_wide,
            sidebar_account_row_height,
            sidebar_account_rail_box_size,
            search_keycap_height,
            task_row_height,
            card_stats_height,
            card_title_height,
            context_menu_row_height,
            tag_height,
            icon_button_size,
            dropdown_item_height,
            textarea_min_height,
            textarea_pref_height
        );

        macro_rules! icons {
            ($($field:ident),+ $(,)?) => {$(
                self.dims.$field = self.icon_size(base.dims.$field);
            )+};
        }
        icons!(
            icon_size_sm,
            icon_size,
            icon_size_md,
            sidebar_rail_icon_glyph,
            sidebar_rail_icon_glyph_wide,
            star_rating_width,
            star_rating_height,
            checkbox_size,
            checkbox_icon_size,
            switch_width,
            switch_height,
            switch_knob,
            switch_radius,
            account_menu_icon_column_width
        );

        macro_rules! paddings {
            ($($field:ident),+ $(,)?) => {$(
                self.dims.$field = self.space(base.dims.$field);
            )+};
        }
        paddings!(
            sidebar_route_spacing,
            account_menu_margin,
            account_menu_bottom_gap,
            account_menu_padding_x,
            account_menu_padding_y,
            account_menu_row_padding_x,
            account_menu_row_padding_y,
            account_menu_update_padding_y,
            account_menu_divider_inset,
            account_menu_footer_gap,
            search_palette_margin,
            search_keycap_padding_x,
            card_padding,
            card_inner_gap,
            card_row_gap,
            context_menu_padding_x,
            context_menu_icon_gap,
            destination_tile_icon_gap,
            destination_row_padding
        );

        // Reading and form bounds grow with text; media dimensions keep their own zoom.
        let reading_scale = self.text_scale_factor.max(1.0);
        macro_rules! reading_bounds {
            ($($field:ident),+ $(,)?) => {$(
                self.dims.$field = base.dims.$field * reading_scale;
            )+};
        }
        reading_bounds!(
            tooltip_max_width,
            account_menu_width,
            search_palette_min_width,
            search_palette_max_width,
            search_palette_input_right_padding,
            settings_modal_width,
            settings_modal_height,
            settings_modal_max_width,
            settings_modal_max_height,
            destination_modal_width,
            destination_modal_max_width,
            destination_history_max_height,
            publish_modal_width,
            publish_modal_max_width,
            publish_modal_max_height,
            preview_modal_width,
            preview_modal_height,
            preview_modal_max_width,
            preview_modal_max_height,
            publish_left_column_width,
            publish_right_column_width,
            publish_changelog_height,
            popup_max_height
        );
        self.dims.scrollbar_thumb_width = between(8.0, 6.0);
        let rail_base = between(60.0, 52.0);
        self.dims.sidebar_rail_width =
            (rail_base * reading_scale).max(self.dims.sidebar_rail_icon_button_size + 16.0);
        self.dims.sidebar_rail_width_inset =
            (84.0 * reading_scale).max(self.dims.sidebar_rail_width);
        self.dims.sidebar_rail_width_wide =
            (96.0 * reading_scale).max(self.dims.sidebar_rail_width_inset + 8.0);
        self.dims.sidebar_account_rail_box_size = self
            .dims
            .sidebar_account_rail_box_size
            .max(self.dims.sidebar_account_rail_avatar_size + 8.0);
        self.dims.sidebar_account_row_height = self
            .dims
            .sidebar_account_row_height
            .max(self.dims.sidebar_account_rail_box_size + 8.0);
        self.dims.card_stats_height = self
            .dims
            .card_stats_height
            .max(self.typography.body * 1.3)
            .max(self.dims.star_rating_height);
        self
    }

    pub(crate) fn layout(&self) -> LayoutMetrics {
        LayoutMetrics {
            spacing: self.spacing,
            radii: self.radii,
            typography: self.typography,
            motion: self.motion,
            dims: self.dims,
            preferred_card_width: self.preferred_card_width,
        }
    }

    pub(crate) fn text_scale(&self) -> f32 {
        self.text_scale_factor
    }
    pub(crate) fn text_size(&self, base: f32) -> f32 {
        (base * self.text_scale_factor).max(11.0)
    }
    pub(crate) fn space(&self, base: f32) -> f32 {
        base * self.spacing_scale_factor
    }
    pub(crate) fn row_height(&self, base: f32) -> f32 {
        base * self.row_scale_factor
    }
    pub(crate) fn icon_size(&self, base: f32) -> f32 {
        base * self.text_scale_factor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn density_changes_spacing_without_changing_text() {
        let compact = Tokens::dark().with_display_preferences(UiDensity::Compact, 125);
        let comfortable = Tokens::dark().with_display_preferences(UiDensity::Comfortable, 125);
        assert_eq!(compact.typography, comfortable.typography);
        assert!(compact.spacing.gap < comfortable.spacing.gap);
        assert!(compact.dims.control_height < comfortable.dims.control_height);
        assert_eq!(compact.layout().typography, compact.typography);
    }

    #[test]
    fn preferences_are_idempotent_and_leave_media_zoom_independent() {
        let original = Tokens::dark();
        let large = original.with_display_preferences(UiDensity::Compact, 150);
        let again = large.with_display_preferences(UiDensity::Compact, 150);
        assert_eq!(large, again);
        assert_eq!(
            large.dims.publish_icon_preview_height,
            original.dims.publish_icon_preview_height
        );
        assert!(large.dims.control_height >= large.typography.body * 1.3);
        assert_eq!(large.motion, original.motion);
    }
}
