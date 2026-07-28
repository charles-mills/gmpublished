use super::{
    Item, Message, RowLayout, State, TitleMeasure, VisibleRowRange, apply, columns_for_width,
    visible_rows_for_viewport,
};
use crate::theme::{self, ThemeVariant, Tokens};
use crate::widgets::addon_card;
use iced::Point;

fn items(heights: &[f32]) -> Vec<Item> {
    heights
        .iter()
        .enumerate()
        .map(|(index, height)| {
            Item::new(addon_card::Data::addon(
                format!("id-{index}"),
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
        vec![
            Message::VisibleRangeChanged(2, 4),
            Message::NextPageRequested
        ]
    );
    assert_eq!(apply(&mut state, Message::Scrolled(260)), Vec::new());
    assert!(state.next_page_was_requested());
}

#[test]
fn unmeasured_viewport_requests_next_page_on_first_scroll() {
    let mut state = State {
        has_more_pages: true,
        ..State::default()
    };
    let _ = state.set_items(items(&[100.0, 100.0, 100.0, 100.0]));

    assert_eq!(
        apply(&mut state, Message::Scrolled(1)),
        vec![Message::NextPageRequested]
    );
    assert_eq!(apply(&mut state, Message::Scrolled(2)), Vec::new());
    assert!(state.next_page_was_requested());
}

#[test]
fn rows_use_addon_card_preferred_height() {
    let tokens = Tokens::dark();
    let card_width = 200.0;
    let items = vec![
        Item::new(addon_card::Data::addon("short", "Short")),
        Item::new(addon_card::Data::addon(
            "long",
            "A long title that needs more height than the short sibling",
        )),
    ];
    let heights = items
        .iter()
        .map(|item| item.preferred_height(card_width, &tokens))
        .collect::<Vec<_>>();

    let layout = RowLayout::for_items(&items, &heights, 2, &tokens);

    assert_eq!(layout.rows().len(), 1);
    assert_eq!(
        layout.rows()[0].content_height(),
        addon_card::preferred_height(items[1].card(), card_width, &tokens)
    );
    assert_eq!(
        layout.rows()[0].height(),
        addon_card::preferred_height(items[1].card(), card_width, &tokens)
            + tokens.dims.card_row_gap
    );
}

#[test]
fn cursor_hover_updates_grid_owned_item_state() {
    let mut state = State::default();
    let _ = apply(&mut state, Message::ViewportResized(500, 500));
    let _ = state.set_items(vec![Item::new(addon_card::Data::addon("id-0", "Addon"))]);
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
    let _ = state.set_items(vec![Item::new(addon_card::Data::addon("id-0", "Addon"))]);
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
    assert_eq!(state.layout_cache_generation(), 0);

    let _ = state.set_items(items(&[100.0]));
    assert_eq!(state.layout_cache_generation(), 1);

    let _ = apply(&mut state, Message::ViewportResized(400, 200));
    assert_eq!(state.layout_cache_generation(), 2);

    let _ = apply(&mut state, Message::ViewportResized(400, 300));
    assert_eq!(state.layout_cache_generation(), 2);

    let _ = apply(&mut state, Message::ColumnsChanged(2));
    assert_eq!(state.layout_cache_generation(), 3);

    let _ = super::view(
        &state,
        &Tokens::for_variant(ThemeVariant::Light),
        "test-grid",
    );
    assert_eq!(state.layout_cache_generation(), 3);

    let _ = state.set_items(items(&[100.0, 120.0]));
    assert_eq!(state.layout_cache_generation(), 4);
}

#[test]
fn patch_items_resolves_by_id_with_the_index_as_a_hint() {
    let mut state = State::default();
    let _ = apply(&mut state, Message::ViewportResized(400, 200));
    let _ = state.set_items(items(&[100.0, 100.0, 100.0]));

    let messages = state.patch_items(vec![
        // Fresh hint: applied directly.
        (
            0,
            Item::new(addon_card::Data::addon("id-0", "Patched")).with_preferred_height(100.0),
        ),
        // Stale hint: the id lives at index 2, not 1 — the patch must land
        // on the item with that id, not the item at the hinted slot.
        (
            1,
            Item::new(addon_card::Data::addon("id-2", "Patched Elsewhere"))
                .with_preferred_height(100.0),
        ),
        // Unknown id: the item left the grid; the patch is dropped.
        (
            1,
            Item::new(addon_card::Data::addon("id-9", "Gone")).with_preferred_height(100.0),
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
    // 400pt wide -> a single column; rows are card height + 10 row gap.
    let _ = apply(&mut state, Message::ViewportResized(400, 200));
    let _ = state.set_items(items(&[100.0; 10]));
    let _ = apply(&mut state, Message::Scrolled(300));
    let anchor_row = state.visible_rows.start;
    let anchor_top = state.layout.top_offset(anchor_row);

    // Row 0 grows by 100 (e.g. a hydrated title re-wrapping taller).
    let messages = state.patch_items(vec![(
        0,
        Item::new(addon_card::Data::addon("id-0", "Addon 0")).with_preferred_height(200.0),
    )]);

    assert_eq!(state.scroll_offset, 400.0);
    // The message carries the *delta*, applied via a relative scroll_by:
    // the mirror offset is u32-quantized by the Scrolled echo, so an
    // absolute scroll_to would drift by the rounding error per anchor.
    assert!(messages.contains(&Message::ScrollAnchored(100.0)));
    // The anchor row sits exactly where it did relative to the viewport.
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
        Item::new(addon_card::Data::addon("id-9", "Addon 9")).with_preferred_height(200.0),
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

    // Duplicate titles share one entry; the override-free Item path measures.
    let _ = state.set_items(vec![
        Item::new(addon_card::Data::addon("id-0", "Alpha")),
        Item::new(addon_card::Data::addon("id-1", "Beta")),
        Item::new(addon_card::Data::addon("id-2", "Alpha")),
    ]);
    let mut titles = state.title_measures.keys().cloned().collect::<Vec<_>>();
    titles.sort();
    assert_eq!(titles, ["Alpha", "Beta"]);
    let alpha_card_height = state.card_heights[0];
    let alpha_measure = state.title_measures["Alpha"];
    let (width_bits, alpha_title_height) = alpha_measure.measured.expect("measured");

    // Reuse proof by poisoning: seed a wrong memo for the current width,
    // re-sync the same title, and the poison must flow into the card
    // height — a re-measure would silently repair it instead. The
    // watermark is dropped so the memo is the only fast path in play.
    let poisoned = TitleMeasure {
        single_line_at: None,
        measured: Some((width_bits, alpha_title_height + 20.0)),
    };
    let _ = state.title_measures.insert("Alpha".to_owned(), poisoned);
    let _ = state.set_items(vec![
        Item::new(addon_card::Data::addon("id-0", "Alpha")),
        Item::new(addon_card::Data::addon("id-1", "Beta")),
    ]);
    assert!((state.card_heights[0] - (alpha_card_height + 20.0)).abs() < 0.01);

    // A memo for a *different* width is dead: after a width change the
    // title is re-measured, repairing the poison.
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
        Item::new(addon_card::Data::addon("id-0", "Alpha")),
        Item::new(addon_card::Data::addon("id-1", long_title)),
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
        Item::new(addon_card::Data::addon("id-0", "Alpha")),
        Item::new(addon_card::Data::addon("id-1", "Beta")),
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
        Item::new(addon_card::Data::addon("id-0", "Disabled").with_enabled(false))
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
        Item::new(addon_card::Data::addon("id-0", "Old title")).with_preferred_height(100.0),
        Item::new(addon_card::Data::addon("id-1", "Other")).with_preferred_height(100.0),
    ]);
    let cursor = card_center(&state, 0);
    let _ = apply(&mut state, Message::CursorMoved(cursor));

    let messages = state.set_items(vec![
        Item::new(addon_card::Data::addon("id-0", "New title")).with_preferred_height(100.0),
        Item::new(addon_card::Data::addon("id-1", "Other")).with_preferred_height(100.0),
    ]);

    assert_eq!(hover_messages(messages), Vec::new());
    assert!(state.items[0].card().is_hovered());
    assert!(!state.items[1].card().is_hovered());
    assert_eq!(state.items[0].card().display_title(), "New title");
}

#[test]
fn visible_card_motion_reports_hover_transitions() {
    let mut state = State::default();
    let _ = state.set_items(vec![Item::new(addon_card::Data::addon("id-0", "Addon"))]);
    let _ = apply(&mut state, Message::ViewportResized(500, 500));
    let started = std::time::Instant::now();

    state.items[0].set_hovered(true, started);

    assert!(state.needs_visible_card_ticks());
    state.tick_visible_card_motion(started + std::time::Duration::from_millis(150));
    assert!(!state.needs_visible_card_ticks());
}
