use super::{
    Item, Message, RowLayout, State, TitleMeasure, VisibleRowRange, apply, columns_for_width,
    visible_rows_for_viewport,
};
use crate::generation::Generation;
use crate::theme::{self, ThemeVariant, Tokens};
use crate::widgets::addon_card;
use crate::widgets::grid_rows::CardId;
use iced::Point;

fn items(heights: &[f32]) -> Vec<Item> {
    heights
        .iter()
        .enumerate()
        .map(|(index, height)| {
            Item::new(addon_card::Data::addon(
                CardId::from(format!("id-{index}")),
                format!("Addon {index}"),
            ))
            .with_preferred_height(*height)
        })
        .collect()
}

fn card_center(state: &State, index: usize) -> Point {
    let columns = state.columns.max(1);
    let row_index = index / columns;
    let col = index % columns;
    let row = &state.layout.rows()[row_index];
    let card_height = state.card_heights[index];
    Point::new(
        col as f32 * (state.card_width + state.card_gap) + state.card_width / 2.0,
        row.top() + card_height / 2.0 - state.scroll_offset,
    )
}

fn hover_messages(messages: Vec<Message>) -> Vec<Message> {
    messages
        .into_iter()
        .filter(|message| matches!(message, Message::CardHoverChanged(_, _)))
        .collect()
}

#[test]
fn columns_follow_iced_responsive_breakpoints() {
    assert_eq!(columns_for_width(0.0), 1);
    assert_eq!(columns_for_width(199.0), 1);
    assert_eq!(columns_for_width(200.0), 1);
    assert_eq!(columns_for_width(400.0), 1);
    assert_eq!(columns_for_width(416.0), 2);
    assert_eq!(columns_for_width(825.0), 3);
    assert_eq!(columns_for_width(848.0), 4);
}

#[test]
fn rows_are_sized_by_their_tallest_card() {
    let gap = theme::invariant().dims.card_row_gap;
    let layout = RowLayout::for_item_heights(&[90.0, 130.0, 75.0, 80.0, 210.0], 2);

    let heights = layout
        .rows()
        .iter()
        .map(super::RowModel::height)
        .collect::<Vec<_>>();
    let content_heights = layout
        .rows()
        .iter()
        .map(super::RowModel::content_height)
        .collect::<Vec<_>>();

    assert_eq!(content_heights, vec![130.0, 80.0, 210.0]);
    assert_eq!(heights, vec![130.0 + gap, 80.0 + gap, 210.0 + gap]);
    assert_eq!(layout.rows()[0].items(), 0..2);
    assert_eq!(layout.rows()[2].items(), 4..5);
    assert_eq!(layout.total_height(), 420.0 + gap * 3.0);
}

#[test]
fn visible_range_uses_per_row_viewport_straddle_without_uniform_height_drift() {
    let layout = RowLayout::for_item_heights(&[100.0, 240.0, 80.0, 60.0, 220.0, 120.0], 1);

    assert_eq!(
        visible_rows_for_viewport(layout.rows(), 230.0, 120.0),
        VisibleRowRange { start: 1, end: 2 }
    );
    assert_eq!(
        visible_rows_for_viewport(layout.rows(), 420.0, 160.0),
        VisibleRowRange { start: 2, end: 5 }
    );
}

#[test]
fn short_final_row_counts_when_list_ends_inside_viewport() {
    let layout = RowLayout::for_item_heights(&[80.0, 80.0, 80.0], 1);

    assert_eq!(
        visible_rows_for_viewport(layout.rows(), 150.0, 400.0),
        VisibleRowRange { start: 1, end: 3 }
    );
}

#[test]
fn state_owns_scroll_offset_and_visible_flat_item_range() {
    let mut state = State::default();
    assert_eq!(
        apply(&mut state, Message::ViewportResized(400, 150)),
        Vec::new()
    );
    assert_eq!(apply(&mut state, Message::ColumnsChanged(1)), Vec::new());
    assert_eq!(
        state.set_items(items(&[120.0, 260.0, 90.0, 90.0, 220.0])),
        vec![Message::VisibleRangeChanged(0, 2)]
    );

    assert_eq!(
        apply(&mut state, Message::Scrolled(270)),
        vec![Message::VisibleRangeChanged(1, 3)]
    );

    assert_eq!(state.scroll_offset(), 270.0);
    assert_eq!(state.visible_rows(), VisibleRowRange { start: 1, end: 3 });
    assert_eq!(state.visible_item_range(), 1..3);
}

#[test]
fn visible_range_changes_are_deduped() {
    let mut state = State::default();
    let _ = state.set_items(items(&[100.0, 100.0, 100.0]));
    assert_eq!(
        apply(&mut state, Message::ViewportResized(200, 150)),
        vec![Message::VisibleRangeChanged(0, 2)]
    );

    assert_eq!(apply(&mut state, Message::Scrolled(0)), Vec::new());
    assert_eq!(apply(&mut state, Message::Scrolled(25)), Vec::new());
    assert_eq!(
        apply(&mut state, Message::Scrolled(116)),
        vec![Message::VisibleRangeChanged(1, 3)]
    );
}

#[test]
fn next_page_request_is_emitted_near_the_end_once() {
    let mut state = State {
        has_more_pages: true,
        ..State::default()
    };
    let _ = state.set_items(items(&[100.0, 100.0, 100.0, 100.0]));
    let _ = apply(&mut state, Message::ViewportResized(200, 150));

    assert_eq!(
        apply(&mut state, Message::Scrolled(250)),
        vec![Message::VisibleRangeChanged(2, 4)]
    );
    assert!(state.take_next_page_request());
    assert_eq!(apply(&mut state, Message::Scrolled(260)), Vec::new());
    assert!(
        !state.take_next_page_request(),
        "the request is claimed once per page"
    );
    assert!(state.next_page_was_requested());
}

#[test]
fn unmeasured_viewport_requests_next_page_on_first_scroll() {
    let mut state = State {
        has_more_pages: true,
        ..State::default()
    };
    let _ = state.set_items(items(&[100.0, 100.0, 100.0, 100.0]));

    assert_eq!(apply(&mut state, Message::Scrolled(1)), Vec::new());
    assert!(state.take_next_page_request());
    assert_eq!(apply(&mut state, Message::Scrolled(2)), Vec::new());
    assert!(
        !state.take_next_page_request(),
        "the request is claimed once per page"
    );
    assert!(state.next_page_was_requested());
}

#[test]
fn rows_use_addon_card_preferred_height() {
    let tokens = theme::invariant();
    let card_width = 200.0;
    let items = vec![
        Item::new(addon_card::Data::addon(CardId::from("short"), "Short")),
        Item::new(addon_card::Data::addon(
            CardId::from("long"),
            "A long title that needs more height than the short sibling",
        )),
    ];
    let heights = items
        .iter()
        .map(|item| item.preferred_height(card_width, tokens))
        .collect::<Vec<_>>();

    let layout = RowLayout::for_items(&items, &heights, 2, tokens);

    assert_eq!(layout.rows().len(), 1);
    assert_eq!(
        layout.rows()[0].content_height(),
        addon_card::preferred_height(items[1].card(), card_width, tokens)
    );
    assert_eq!(
        layout.rows()[0].height(),
        addon_card::preferred_height(items[1].card(), card_width, tokens)
            + tokens.dims.card_row_gap
    );
}

#[test]
fn cursor_hover_updates_grid_owned_item_state() {
    let mut state = State::default();
    let _ = apply(&mut state, Message::ViewportResized(500, 500));
    let _ = state.set_items(vec![Item::new(addon_card::Data::addon(
        CardId::from("id-0"),
        "Addon",
    ))]);
    let cursor = card_center(&state, 0);

    assert_eq!(
        apply(&mut state, Message::CursorMoved(cursor)),
        vec![Message::CardHoverChanged("id-0".into(), true)]
    );

    assert!(state.items[0].card().is_hovered());
}

#[test]
fn resize_shuffle_reconciles_hover_to_card_under_stationary_cursor() {
    let mut state = State::default();
    state.metrics.motion.reduce_motion = true;
    let _ = state.set_items(items(&[100.0, 100.0, 100.0, 100.0]));
    let _ = apply(&mut state, Message::ViewportResized(200, 500));
    let cursor = card_center(&state, 1);

    assert_eq!(
        apply(&mut state, Message::CursorMoved(cursor)),
        vec![Message::CardHoverChanged("id-1".into(), true)]
    );

    let messages = apply(&mut state, Message::ViewportResized(500, 500));

    assert_eq!(
        hover_messages(messages),
        vec![
            Message::CardHoverChanged("id-1".into(), false),
            Message::CardHoverChanged("id-2".into(), true),
        ]
    );
    assert!(!state.items[1].card().is_hovered());
    assert!(state.items[2].card().is_hovered());
}

#[test]
fn scroll_reconcile_moves_hover_with_stationary_cursor_and_clears_misses() {
    let mut state = State::default();
    let _ = state.set_items(items(&[100.0, 100.0, 100.0, 100.0]));
    let _ = apply(&mut state, Message::ViewportResized(200, 500));
    let cursor = card_center(&state, 0);
    let row_height = { state.layout.rows()[0].height() };

    assert_eq!(
        apply(&mut state, Message::CursorMoved(cursor)),
        vec![Message::CardHoverChanged("id-0".into(), true)]
    );

    let messages = apply(&mut state, Message::Scrolled(row_height as u32));

    assert_eq!(
        hover_messages(messages),
        vec![
            Message::CardHoverChanged("id-0".into(), false),
            Message::CardHoverChanged("id-1".into(), true),
        ]
    );
    assert!(!state.items[0].card().is_hovered());
    assert!(state.items[1].card().is_hovered());

    let messages = apply(&mut state, Message::Scrolled(1_000));

    assert_eq!(
        hover_messages(messages),
        vec![Message::CardHoverChanged("id-1".into(), false)]
    );
    assert!(state.items.iter().all(|item| !item.card().is_hovered()));
}

#[test]
fn cursor_left_clears_hover() {
    let mut state = State::default();
    let _ = apply(&mut state, Message::ViewportResized(500, 500));
    let _ = state.set_items(vec![Item::new(addon_card::Data::addon(
        CardId::from("id-0"),
        "Addon",
    ))]);
    let cursor = card_center(&state, 0);
    let _ = apply(&mut state, Message::CursorMoved(cursor));

    assert_eq!(
        apply(&mut state, Message::CursorLeft),
        vec![Message::CardHoverChanged("id-0".into(), false)]
    );

    assert!(!state.items[0].card().is_hovered());
}

#[test]
fn gap_and_past_last_row_miss_hover_targets() {
    let mut state = State::default();
    let _ = state.set_items(items(&[100.0, 100.0]));
    let _ = apply(&mut state, Message::ViewportResized(500, 500));
    let layout = &state.layout;
    let row = &layout.rows()[0];
    let gap = Point::new(
        state.card_width + state.card_gap / 2.0,
        row.top() + row.height() / 2.0,
    );
    let below_content_gap = Point::new(state.card_width / 2.0, state.card_heights[0] + 1.0);
    let past_last_row = Point::new(state.card_width / 2.0, layout.total_height() + 1.0);

    assert_eq!(apply(&mut state, Message::CursorMoved(gap)), Vec::new());
    assert_eq!(
        apply(&mut state, Message::CursorMoved(below_content_gap)),
        Vec::new()
    );
    assert_eq!(
        apply(&mut state, Message::CursorMoved(past_last_row)),
        Vec::new()
    );
    assert!(state.items.iter().all(|item| !item.card().is_hovered()));
}

#[test]
fn hover_uses_each_cards_own_cached_height_inside_a_mixed_row() {
    let mut state = State::default();
    let _ = state.set_items(items(&[100.0, 160.0]));
    let _ = apply(&mut state, Message::ViewportResized(500, 500));
    let row_top = state.layout.rows()[0].top();
    let y = row_top + 120.0;
    let short_center_x = state.card_width / 2.0;
    let short_column = Point::new(state.card_width / 2.0, y);
    let tall_column = Point::new(
        state.card_width + state.card_gap + state.card_width / 2.0,
        y,
    );

    assert_eq!(
        apply(
            &mut state,
            Message::CursorMoved(Point::new(short_center_x, row_top + 50.0))
        ),
        vec![Message::CardHoverChanged("id-0".into(), true)]
    );
    assert_eq!(
        hover_messages(apply(&mut state, Message::CursorMoved(short_column))),
        vec![Message::CardHoverChanged("id-0".into(), false)]
    );
    assert_eq!(
        hover_messages(apply(&mut state, Message::CursorMoved(tall_column))),
        vec![Message::CardHoverChanged("id-1".into(), true)]
    );
}

#[test]
fn layout_cache_recomputes_only_for_items_width_and_columns() {
    let mut state = State::default();
    assert_eq!(state.layout_cache_generation(), Generation::from_raw(0));

    let _ = state.set_items(items(&[100.0]));
    assert_eq!(state.layout_cache_generation(), Generation::from_raw(1));

    let _ = apply(&mut state, Message::ViewportResized(400, 200));
    assert_eq!(state.layout_cache_generation(), Generation::from_raw(2));

    let _ = apply(&mut state, Message::ViewportResized(400, 300));
    assert_eq!(state.layout_cache_generation(), Generation::from_raw(2));

    let _ = apply(&mut state, Message::ColumnsChanged(2));
    assert_eq!(state.layout_cache_generation(), Generation::from_raw(3));

    let _ = super::view(
        &state,
        &Tokens::for_variant(ThemeVariant::Light),
        "test-grid",
    );
    assert_eq!(state.layout_cache_generation(), Generation::from_raw(3));

    let _ = state.set_items(items(&[100.0, 120.0]));
    assert_eq!(state.layout_cache_generation(), Generation::from_raw(4));
}

#[test]
fn patch_items_resolves_by_id_with_the_index_as_a_hint() {
    let mut state = State::default();
    let _ = apply(&mut state, Message::ViewportResized(400, 200));
    let _ = state.set_items(items(&[100.0, 100.0, 100.0]));

    let messages = state.patch_items(vec![
        (
            0,
            Item::new(addon_card::Data::addon(CardId::from("id-0"), "Patched"))
                .with_preferred_height(100.0),
        ),
        (
            1,
            Item::new(addon_card::Data::addon(
                CardId::from("id-2"),
                "Patched Elsewhere",
            ))
            .with_preferred_height(100.0),
        ),
        (
            1,
            Item::new(addon_card::Data::addon(CardId::from("id-9"), "Gone"))
                .with_preferred_height(100.0),
        ),
    ]);

    assert_eq!(messages, Vec::new());
    assert_eq!(state.items[0].card().display_title(), "Patched");
    assert_eq!(state.items[1].card().display_title(), "Addon 1");
    assert_eq!(state.items[2].card().display_title(), "Patched Elsewhere");
}

#[test]
fn patch_items_anchors_scroll_when_heights_above_the_viewport_change() {
    let mut state = State::default();
    let _ = apply(&mut state, Message::ViewportResized(400, 200));
    let _ = state.set_items(items(&[100.0; 10]));
    let _ = apply(&mut state, Message::Scrolled(300));
    let anchor_row = state.visible_rows.start;
    let anchor_top = state.layout.top_offset(anchor_row);

    let messages = state.patch_items(vec![(
        0,
        Item::new(addon_card::Data::addon(CardId::from("id-0"), "Addon 0"))
            .with_preferred_height(200.0),
    )]);

    assert_eq!(state.scroll_offset, 400.0);
    assert!(messages.contains(&Message::ScrollAnchored(100.0)));
    assert_eq!(
        state.layout.top_offset(anchor_row) - state.scroll_offset,
        anchor_top - 300.0
    );
}

#[test]
fn patch_items_does_not_anchor_when_heights_change_below_the_viewport() {
    let mut state = State::default();
    let _ = apply(&mut state, Message::ViewportResized(400, 200));
    let _ = state.set_items(items(&[100.0; 10]));
    let _ = apply(&mut state, Message::Scrolled(300));

    let messages = state.patch_items(vec![(
        9,
        Item::new(addon_card::Data::addon(CardId::from("id-9"), "Addon 9"))
            .with_preferred_height(200.0),
    )]);

    assert_eq!(state.scroll_offset, 300.0);
    assert!(
        !messages
            .iter()
            .any(|message| matches!(message, Message::ScrollAnchored(_)))
    );
}

#[test]
fn title_measure_memo_is_reused_at_the_same_width_and_ignored_at_another() {
    let mut state = State::default();
    let _ = apply(&mut state, Message::ViewportResized(500, 500));

    let _ = state.set_items(vec![
        Item::new(addon_card::Data::addon(CardId::from("id-0"), "Alpha")),
        Item::new(addon_card::Data::addon(CardId::from("id-1"), "Beta")),
        Item::new(addon_card::Data::addon(CardId::from("id-2"), "Alpha")),
    ]);
    let mut titles = state.title_measures.keys().cloned().collect::<Vec<_>>();
    titles.sort();
    assert_eq!(titles, ["Alpha", "Beta"]);
    let alpha_card_height = state.card_heights[0];
    let alpha_measure = state.title_measures["Alpha"].clone();
    let (width_bits, alpha_title_height) = alpha_measure.measured.expect("measured");

    // Seed an incorrect height and remove the watermark. Reusing the memo must
    // preserve that height; measuring again would repair it.
    let poisoned = TitleMeasure {
        paragraph: None,
        single_line_at: None,
        measured: Some((width_bits, alpha_title_height + 20.0)),
    };
    let _ = state.title_measures.insert("Alpha".to_owned(), poisoned);
    let _ = state.set_items(vec![
        Item::new(addon_card::Data::addon(CardId::from("id-0"), "Alpha")),
        Item::new(addon_card::Data::addon(CardId::from("id-1"), "Beta")),
    ]);
    assert!((state.card_heights[0] - (alpha_card_height + 20.0)).abs() < 0.01);

    let _ = apply(&mut state, Message::ViewportResized(900, 500));
    let repaired = state.title_measures["Alpha"].measured.expect("measured");
    assert_ne!(repaired.0, width_bits, "memo must be for the new width");
    assert!((repaired.1 - alpha_title_height).abs() < 0.5);
}

#[test]
fn title_measure_watermark_skips_shaping_at_wider_widths() {
    let mut state = State::default();
    let _ = apply(&mut state, Message::ViewportResized(500, 500));
    let long_title = "This Genuinely Long Addon Title Certainly Wraps Onto Several Lines";
    let _ = state.set_items(vec![
        Item::new(addon_card::Data::addon(CardId::from("id-0"), "Alpha")),
        Item::new(addon_card::Data::addon(CardId::from("id-1"), long_title)),
    ]);
    assert!(
        state.card_heights[1] > state.card_heights[0],
        "fixture title must actually be multi-line"
    );

    // Watermark proof by poisoning: claim the long title is single-line at
    // any width and drop its memo. If the watermark short-circuits shaping
    // (as designed), the next width change lays it out one line tall; a
    // re-measure would find its real multi-line height instead.
    let _ = state.title_measures.insert(
        long_title.to_owned(),
        TitleMeasure {
            paragraph: None,
            single_line_at: Some(0.0),
            measured: None,
        },
    );
    let _ = apply(&mut state, Message::ViewportResized(900, 500));
    assert!((state.card_heights[1] - state.card_heights[0]).abs() < 0.01);
}

#[test]
fn title_measure_cache_compacts_against_title_churn() {
    let mut state = State::default();
    let _ = apply(&mut state, Message::ViewportResized(500, 500));
    for index in 0..200 {
        let _ = state
            .title_measures
            .insert(format!("Stale Title {index}"), TitleMeasure::default());
    }

    let _ = state.set_items(vec![
        Item::new(addon_card::Data::addon(CardId::from("id-0"), "Alpha")),
        Item::new(addon_card::Data::addon(CardId::from("id-1"), "Beta")),
    ]);

    let mut titles = state.title_measures.keys().cloned().collect::<Vec<_>>();
    titles.sort();
    assert_eq!(titles, ["Alpha", "Beta"]);
}

#[test]
fn disabled_card_is_not_hoverable() {
    let mut state = State::default();
    let _ = apply(&mut state, Message::ViewportResized(500, 500));
    let _ = state.set_items(vec![
        Item::new(addon_card::Data::addon(CardId::from("id-0"), "Disabled").with_enabled(false))
            .with_preferred_height(100.0),
    ]);
    let cursor = card_center(&state, 0);

    assert_eq!(apply(&mut state, Message::CursorMoved(cursor)), Vec::new());

    assert!(!state.items[0].card().is_hovered());
}

#[test]
fn set_items_keeps_still_valid_hover_without_notifications() {
    let mut state = State::default();
    let _ = apply(&mut state, Message::ViewportResized(500, 500));
    let _ = state.set_items(vec![
        Item::new(addon_card::Data::addon(CardId::from("id-0"), "Old title"))
            .with_preferred_height(100.0),
        Item::new(addon_card::Data::addon(CardId::from("id-1"), "Other"))
            .with_preferred_height(100.0),
    ]);
    let cursor = card_center(&state, 0);
    let _ = apply(&mut state, Message::CursorMoved(cursor));

    let messages = state.set_items(vec![
        Item::new(addon_card::Data::addon(CardId::from("id-0"), "New title"))
            .with_preferred_height(100.0),
        Item::new(addon_card::Data::addon(CardId::from("id-1"), "Other"))
            .with_preferred_height(100.0),
    ]);

    assert_eq!(hover_messages(messages), Vec::new());
    assert!(state.items[0].card().is_hovered());
    assert!(!state.items[1].card().is_hovered());
    assert_eq!(state.items[0].card().display_title(), "New title");
}

#[test]
fn visible_card_motion_reports_hover_transitions() {
    let mut state = State::default();
    let _ = state.set_items(vec![Item::new(addon_card::Data::addon(
        CardId::from("id-0"),
        "Addon",
    ))]);
    let _ = apply(&mut state, Message::ViewportResized(500, 500));
    let started = std::time::Instant::now();

    state.items[0].set_hovered(true, started);

    assert!(state.needs_visible_card_ticks());
    state.tick_visible_card_motion(started + std::time::Duration::from_millis(150));
    assert!(!state.needs_visible_card_ticks());
}

#[test]
fn density_changes_columns_without_sizing_cards_from_item_count() {
    let mut state = State::default();
    let _ = apply(&mut state, Message::ViewportResized(1800, 600));
    let mut metrics = *theme::invariant();
    metrics.preferred_card_width = 280.0;
    metrics.spacing.gap = 24.0;
    let _ = state.set_metrics(&metrics);
    let _ = state.set_items(items(&[100.0; 2]));
    let sparse_width = state.card_width;
    assert_eq!(state.columns, 5);
    assert!((280.0..=378.0).contains(&sparse_width));
    let _ = state.set_items(items(&[100.0; 100]));
    assert_eq!(state.card_width, sparse_width);

    metrics.preferred_card_width = 220.0;
    metrics.spacing.gap = 16.0;
    let _ = state.set_metrics(&metrics);
    assert_eq!(state.columns, 7);
    assert!(state.card_width < sparse_width);
}

#[test]
fn display_change_preserves_visible_item_and_scroll_inset_across_column_change() {
    let mut state = State::default();
    let _ = apply(&mut state, Message::ViewportResized(900, 160));
    let _ = state.set_items(items(&[100.0; 100]));
    let _ = apply(&mut state, Message::Scrolled(345));
    let anchor_item = state.visible_item_range().start;
    let inset = state.scroll_offset - state.layout.top_offset(state.visible_rows.start);
    let mut metrics = state.metrics;
    metrics.preferred_card_width = 280.0;
    metrics.spacing.gap = 24.0;
    metrics.dims.card_row_gap = 24.0;
    let messages = state.set_metrics(&metrics);
    let row = anchor_item / state.columns;
    assert!((state.scroll_offset - state.layout.top_offset(row) - inset).abs() < 0.01);
    assert!(state.visible_item_range().contains(&anchor_item));
    assert!(
        messages
            .iter()
            .any(|message| matches!(message, Message::ScrollAnchored(_)))
    );
}

#[test]
fn changing_typography_invalidates_title_measures_and_noop_changes_are_free() {
    let mut state = State::default();
    let _ = apply(&mut state, Message::ViewportResized(900, 500));
    let _ = state.set_items(vec![Item::new(addon_card::Data::addon(
        CardId::from("a"),
        "Alpha",
    ))]);
    let original_height = state.card_heights[0];
    let original_generation = state.layout_cache_generation();
    let mut metrics = state.metrics;
    metrics.typography.body *= 1.5;
    let _ = state.set_metrics(&metrics);
    assert!(state.card_heights[0] > original_height);
    assert!(
        state.title_measures["Alpha"].measured.unwrap().1
            > addon_card::single_line_title_height(theme::invariant())
    );
    assert_ne!(state.layout_cache_generation(), original_generation);
    let generation = state.layout_cache_generation();
    assert!(state.set_metrics(&metrics).is_empty());
    assert_eq!(state.layout_cache_generation(), generation);
}

#[test]
fn display_change_reconciles_hover_using_the_new_column_geometry() {
    let mut state = State::default();
    state.metrics.motion.reduce_motion = true;
    let _ = apply(&mut state, Message::ViewportResized(500, 500));
    let _ = state.set_items(items(&[100.0; 4]));
    let cursor = card_center(&state, 1);
    let _ = apply(&mut state, Message::CursorMoved(cursor));
    assert!(state.items[1].card().is_hovered());
    let mut metrics = state.metrics;
    metrics.preferred_card_width = 400.0;
    let messages = state.set_metrics(&metrics);
    assert_eq!(state.columns, 1);
    assert_eq!(
        hover_messages(messages),
        vec![
            Message::CardHoverChanged("id-1".into(), false),
            Message::CardHoverChanged("id-0".into(), true),
        ]
    );
    assert!(state.items[0].card().is_hovered());
}

#[test]
#[ignore = "opt-in visual fixtures; requires a headless graphics backend"]
fn capture_adaptive_gallery_fixtures() {
    use crate::bridge::UiDensity;
    use iced::widget::{column, container, image, row, text};
    use iced::{Color, Length};
    use std::borrow::Cow;

    let output = std::env::var_os("GMPUBLISHED_UI_CAPTURE_DIR").map_or_else(
        || "/tmp/gmpublished-ui-refactor".into(),
        std::path::PathBuf::from,
    );
    std::fs::create_dir_all(&output).expect("capture directory");
    let settings = || iced_test::core::Settings {
        default_font: crate::assets::fonts::default_font(),
        fonts: crate::assets::fonts::bundled_fonts()
            .into_iter()
            .map(Cow::Borrowed)
            .collect(),
        ..Default::default()
    };
    // Register the real bundled fonts before measuring any virtualized titles.
    let _prime = iced_test::Simulator::<Message>::with_settings(settings(), text("Fonts"));
    let titles = [
        "rRadio (Boombox and Car Radio)",
        "Hedge Networks Custom Content",
        "PERFOPUS: Performance Metrics",
        "An unusually long Workshop title that wraps across several lines and remains available in full through addon details",
        "日本語のアドオン · 中文内容 · 한국어 애드온",
        "Boombox (Model Only)",
    ];
    for (name, density, percent, width, height, count) in [
        (
            "comfortable-sparse",
            UiDensity::Comfortable,
            100,
            1920,
            1080,
            4,
        ),
        (
            "comfortable-library",
            UiDensity::Comfortable,
            100,
            2560,
            1440,
            200,
        ),
        (
            "compact-large-text-sparse",
            UiDensity::Compact,
            150,
            1280,
            800,
            4,
        ),
        (
            "compact-large-text-library",
            UiDensity::Compact,
            150,
            1920,
            1080,
            200,
        ),
        (
            "compact-large-text-narrow",
            UiDensity::Compact,
            150,
            800,
            600,
            24,
        ),
    ] {
        let tokens = Tokens::dark().with_display_preferences(density, percent);
        let mut state = State::default();
        let _ = state.set_metrics(&tokens.layout());
        let gutter = tokens.spacing.pad;
        let content_height =
            height as f32 - 2.0 * gutter - tokens.typography.title_lg * 1.3 - tokens.spacing.gap;
        let _ = apply(
            &mut state,
            Message::ViewportResized((width as f32 - 2.0 * gutter) as u32, content_height as u32),
        );
        let _ = state.set_items(
            (0..count)
                .map(|index| {
                    let rgb = [
                        (36 + index * 31 % 130) as u8,
                        (70 + index * 17 % 120) as u8,
                        (120 + index * 13 % 120) as u8,
                    ];
                    let mut rgba = Vec::with_capacity(64 * 64 * 4);
                    for y in 0..64 {
                        for x in 0..64 {
                            let shade = if (x / 16 + y / 16) % 2 == 0 { 1.0 } else { 0.7 };
                            rgba.extend([
                                (rgb[0] as f32 * shade) as u8,
                                (rgb[1] as f32 * shade) as u8,
                                (rgb[2] as f32 * shade) as u8,
                                255,
                            ]);
                        }
                    }
                    Item::new(
                        addon_card::Data::addon(
                            CardId::from(format!("capture-{index}")),
                            titles[index % titles.len()],
                        )
                        .with_subscriptions("77,832", 77832)
                        .with_score((index % 6) as i32, "100%")
                        .with_thumbnail(addon_card::Thumbnail::Ready(image::Handle::from_rgba(
                            64, 64, rgba,
                        ))),
                    )
                })
                .collect(),
        );
        let frame = container(
            column![
                row![
                    text("My Workshop").size(tokens.typography.title_lg),
                    text(count.to_string())
                        .size(tokens.typography.body)
                        .color(Color::from(tokens.colors.text_dim))
                ]
                .spacing(tokens.spacing.gap_sm)
                .align_y(iced::Center),
                super::view(&state, &tokens, "gallery-capture"),
            ]
            .spacing(tokens.spacing.gap)
            .width(Length::Fill)
            .height(Length::Fill),
        )
        .padding(gutter)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(Color::from(tokens.colors.bg).into()),
            ..Default::default()
        });
        let mut simulation = iced_test::Simulator::<Message>::with_size(
            settings(),
            (width as f32, height as f32),
            frame,
        );
        let snapshot = simulation
            .snapshot(&iced::Theme::Dark)
            .expect("gallery render");
        assert!(
            snapshot
                .matches_image(output.join(name))
                .expect("save capture")
        );
        println!("gallery capture: {}", output.join(name).display());
    }
}

/// Wall-clock observations use the same fixed 10k-card fixture in legacy and
/// resolved metrics. This is an opt-in report, avoiding brittle timing limits in CI.
#[test]
#[ignore = "opt-in large-library timing report"]
fn report_large_library_geometry_timing() {
    use crate::bridge::UiDensity;
    use std::time::Instant;
    let fixtures: Vec<_> = (0..10_000)
        .map(|index| {
            Item::new(addon_card::Data::addon(
                CardId::from(format!("perf-{index}")),
                format!("Workshop addon {} with a representative title", index % 100),
            ))
        })
        .collect();
    for (name, metrics) in [
        ("legacy", *theme::invariant()),
        (
            "comfortable-100",
            Tokens::dark()
                .with_display_preferences(UiDensity::Comfortable, 100)
                .layout(),
        ),
        (
            "compact-150",
            Tokens::dark()
                .with_display_preferences(UiDensity::Compact, 150)
                .layout(),
        ),
    ] {
        let mut state = State::default();
        let _ = state.set_metrics(&metrics);
        let _ = apply(&mut state, Message::ViewportResized(1920, 1080));
        let started = Instant::now();
        let _ = state.set_items(fixtures.clone());
        let build = started.elapsed();
        let started = Instant::now();
        for offset in (0..100_000).step_by(100) {
            let _ = apply(&mut state, Message::Scrolled(offset));
        }
        let scroll = started.elapsed();
        let started = Instant::now();
        for width in (1280..=2560).step_by(64) {
            let _ = apply(&mut state, Message::ViewportResized(width, 1080));
        }
        let resize = started.elapsed();
        println!(
            "{name}: 10k-card build {build:?}, 1000 scrolls {scroll:?}, 21 resizes {resize:?}"
        );
    }
}

#[test]
fn column_changes_report_new_item_demand_even_when_visible_row_range_is_unchanged() {
    let mut state = State::default();
    let _ = apply(&mut state, Message::ViewportResized(1800, 150));
    let _ = state.set_items(items(&[100.0; 100]));
    let old_rows = state.visible_rows;
    let old_items = state.visible_item_range();
    let mut metrics = state.metrics;
    metrics.preferred_card_width = 280.0;
    let messages = state.set_metrics(&metrics);
    assert_eq!(state.visible_rows, old_rows);
    assert_ne!(state.visible_item_range(), old_items);
    assert!(messages.contains(&Message::VisibleRangeChanged(old_rows.start, old_rows.end)));
}

#[test]
fn column_reflow_bounds_cross_row_travel_then_settles_and_reconciles_hover() {
    use std::time::{Duration, Instant};
    let now = Instant::now();
    let mut state = State::default();
    let _ = state.set_items(items(&[100.0; 20]));
    let _ = state.set_viewport_size_at(iced::Size::new(200.0, 500.0), now);
    let original = state.card_position(1).unwrap();
    state.cursor = Some(original + iced::Vector::new(20.0, 20.0));
    let _ = state.reconcile_hover(now);
    let _ = state.set_viewport_size_at(iced::Size::new(500.0, 500.0), now);
    let id = state.items[1].id().clone();
    let animated = state.card_position(1).unwrap() + state.reflow_offset(&id, now);
    assert_ne!(animated, original);
    assert_eq!(state.reflow_offset(&id, now), iced::Vector::new(0.0, 6.0));
    assert!(state.reflow_opacity(&id, now) < 1.0);
    state.cursor = Some(animated + iced::Vector::new(20.0, 20.0));
    let _ = state.reconcile_hover(now);
    assert_eq!(state.hover_target_id_at(now), Some(&id));
    assert!(state.needs_visible_card_ticks());
    assert!(state.reflow_offsets.len() < state.items.len());
    let midpoint = now + Duration::from_millis(90);
    let halfway = state.reflow_offset(&id, midpoint);
    assert!(halfway.y.abs() < state.reflow_offset(&id, now).y.abs());
    state.cursor = Some(iced::Point::new(-1.0, -1.0));
    let messages = state.tick_visible_card_motion(now + Duration::from_millis(200));
    assert!(state.reflow_offsets.is_empty());
    assert!(!state.reflow_motion.needs_ticks());
    assert!(
        messages
            .iter()
            .any(|message| matches!(message, Message::CardHoverChanged(_, false)))
    );
}

#[test]
fn empty_collections_release_shaped_title_cache() {
    let mut state = State::default();
    let _ = state.set_items(vec![Item::new(addon_card::Data::addon(
        "cached".into(),
        "Cached title",
    ))]);
    assert!(!state.title_measures.is_empty());
    let _ = state.set_items(Vec::new());
    assert!(state.title_measures.is_empty());
}

#[test]
fn reduced_motion_skips_reflow_and_thumbnail_hover_ticks() {
    let mut state = State::default();
    let _ = state.set_items(items(&[100.0; 20]));
    let _ = apply(&mut state, Message::ViewportResized(200, 500));
    let _ = apply(&mut state, Message::ViewportResized(500, 500));
    assert!(state.reflow_motion.needs_ticks());
    let mut metrics = state.metrics;
    metrics.motion.reduce_motion = true;
    let _ = state.set_metrics(&metrics);
    assert!(state.reflow_offsets.is_empty());
    assert!(!state.needs_visible_card_ticks());
}

#[test]
fn single_line_title_cache_survives_narrower_cards_that_still_fit() {
    let mut measures = std::collections::HashMap::new();
    let metrics = theme::invariant();
    let height = super::cached_title_height("Short", 360.0, metrics, &mut measures);
    let threshold = measures["Short"].single_line_at.unwrap();
    assert!(threshold < 200.0);
    assert_eq!(
        super::cached_title_height("Short", 200.0, metrics, &mut measures),
        height
    );
    assert_eq!(
        measures["Short"].measured.unwrap().0,
        (360.0 - metrics.dims.card_padding * 2.0).to_bits()
    );
}

#[test]
fn transitional_offscreen_title_memos_are_remeasured_on_settlement() {
    let mut state = State::default();
    let _ = apply(&mut state, Message::ViewportResized(900, 500));
    let data = (0..500)
        .map(|index| {
            Item::new(addon_card::Data::addon(
                format!("id-{index}").into(),
                format!(
                    "A long title that will need several lines when cards become narrower {index}"
                ),
            ))
        })
        .collect();
    let _ = state.set_items(data);
    let title = state.items[400].card.display_title().to_owned();
    let mut metrics = state.metrics;
    metrics.motion.layout_animating = true;
    metrics.preferred_card_width = 280.0;
    let previous = state.title_measures[&title].measured.unwrap();
    let _ = state.set_metrics(&metrics);
    assert_eq!(state.title_measures[&title].measured.unwrap(), previous);
    for index in state.render_item_range() {
        assert_eq!(
            state.card_heights[index],
            state.items[index].preferred_height(state.card_width, &metrics)
        );
    }
    metrics.motion.layout_animating = false;
    let _ = state.set_metrics(&metrics);
    assert_ne!(state.title_measures[&title].measured.unwrap().0, previous.0);
    assert_eq!(
        state.card_heights[400],
        state.items[400].preferred_height(state.card_width, &metrics)
    );
}

#[test]
#[ignore = "opt-in density motion timing report"]
fn report_large_library_density_motion_timing() {
    use std::time::Instant;
    let mut state = State::default();
    let _ = state.set_metrics(&Tokens::dark().with_density_progress(0.0, 100).layout());
    let _ = apply(&mut state, Message::ViewportResized(1920, 1080));
    let _ = state.set_items(
        (0..10_000)
            .map(|index| {
                Item::new(addon_card::Data::addon(
                    format!("perf-{index}").into(),
                    format!("Workshop addon {index} with a representative title"),
                ))
            })
            .collect(),
    );
    let mut frames = Vec::new();
    for index in 1..=12 {
        let mut metrics = Tokens::dark()
            .with_density_progress(index as f32 / 12.0, 100)
            .layout();
        metrics.motion.layout_animating = index < 12;
        let started = Instant::now();
        let _ = state.set_metrics(&metrics);
        frames.push(started.elapsed());
    }
    println!("10k distinct-title density layout frames: {frames:?}");
    println!(
        "Visible card reflow offsets: {} / {} library items",
        state.reflow_offsets.len(),
        state.items.len()
    );
}

#[test]
#[ignore = "opt-in gallery motion frames; requires a headless graphics backend"]
fn capture_gallery_reflow_motion_frames() {
    use iced::widget::{column, container, image, text};
    use iced::{Color, Length, Size};
    use std::time::{Duration, Instant};
    for font in crate::assets::fonts::bundled_fonts() {
        iced_test::renderer::graphics::text::font_system()
            .write()
            .unwrap()
            .load_font(std::borrow::Cow::Borrowed(font));
    }
    let tokens = Tokens::dark().with_density_progress(1.0, 100);
    let mut state = State::default();
    let _ = state.set_metrics(&tokens.layout());
    let started = Instant::now();
    let _ = state.set_viewport_size_at(Size::new(1100.0, 640.0), started);
    let _ = state.set_items(
        (0..100)
            .map(|index| {
                let rgb = [
                    70 + (index % 6 * 20) as u8,
                    85 + (index % 4 * 24) as u8,
                    110 + (index % 5 * 18) as u8,
                    255,
                ];
                let pixels = rgb.repeat(128 * 128);
                Item::new(
                    addon_card::Data::addon(
                        format!("motion-{index}").into(),
                        format!("Workshop addon {}", index + 1),
                    )
                    .with_thumbnail(addon_card::Thumbnail::Ready(
                        image::Handle::from_rgba(128, 128, pixels),
                    )),
                )
            })
            .collect(),
    );
    let _ = state.set_viewport_size_at(Size::new(1500.0, 640.0), started);
    for elapsed in [0, 45, 90, 180, 240] {
        let time = started + Duration::from_millis(elapsed);
        let gallery = super::view_at(&state, &tokens, "motion-capture", time);
        let frame = container(
            column![
                text(format!("Library reflow · {elapsed} ms")).size(tokens.typography.title),
                gallery,
            ]
            .spacing(tokens.spacing.gap)
            .height(Length::Fill),
        )
        .padding(tokens.spacing.pad)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(Color::from_rgb8(26, 26, 26).into()),
            text_color: Some(Color::WHITE),
            ..Default::default()
        });
        crate::test_support::capture_ui_surface_at_scale(
            &format!("gallery-reflow-{elapsed:03}ms"),
            Size::new(1556.0, 720.0),
            1.0,
            &iced::Theme::Dark,
            frame.into(),
        );
    }
}
