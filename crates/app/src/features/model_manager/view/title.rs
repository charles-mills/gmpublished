use std::cell::RefCell;
use std::collections::HashMap;

use iced::advanced::graphics::text::Paragraph;
use iced::advanced::text::Paragraph as _;
use iced::widget::{responsive, text};
use iced::{Element, Length, Pixels, Size};
use unicode_segmentation::UnicodeSegmentation as _;

use super::super::Message;
use crate::theme::{self, ViewCtx};
use crate::widgets::tooltip;

thread_local! {
    static LABELS: RefCell<HashMap<(String, u32, u32), String>> = RefCell::default();
}

pub(super) fn view(title: String, ctx: ViewCtx<'_>) -> Element<'_, Message> {
    let tokens = *ctx.tokens;
    let label = title.clone();
    tooltip::below(
        responsive(move |bounds| {
            text(fit(&label, bounds.width, tokens.typography.body))
                .size(tokens.typography.body)
                .font(theme::styles::inter_font(iced::font::Weight::Semibold))
                .wrapping(text::Wrapping::None)
                .into()
        })
        .width(Length::Fill)
        .height(tokens.text_size(22.0)),
        title,
        &tokens,
        tokens.text_size(420.0),
    )
}

fn fit(title: &str, width: f32, size: f32) -> String {
    let key = (title.to_owned(), width.to_bits(), size.to_bits());
    if let Some(label) = LABELS.with(|labels| labels.borrow().get(&key).cloned()) {
        return label;
    }
    let measure = |content: &str| {
        Paragraph::with_text(iced::advanced::Text {
            content,
            bounds: Size::INFINITE,
            size: Pixels(size),
            line_height: iced::advanced::text::LineHeight::default(),
            font: theme::styles::inter_font(iced::font::Weight::Semibold),
            align_x: iced::advanced::text::Alignment::Default,
            align_y: iced::alignment::Vertical::Top,
            shaping: iced::advanced::text::Shaping::default(),
            wrapping: iced::advanced::text::Wrapping::None,
        })
        .min_bounds()
        .width
    };
    let label = if measure(title) <= width {
        title.to_owned()
    } else if measure("…") > width {
        String::new()
    } else {
        let boundaries: Vec<_> = title
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .collect();
        let mut lower = 0;
        let mut upper = boundaries.len().saturating_sub(1);
        while lower < upper {
            let middle = (lower + upper).div_ceil(2);
            if measure(&format!("{}…", &title[..boundaries[middle]])) <= width {
                lower = middle;
            } else {
                upper = middle - 1;
            }
        }
        format!("{}…", &title[..boundaries[lower]])
    };
    LABELS.with(|labels| {
        let mut labels = labels.borrow_mut();
        if labels.len() >= 128 {
            labels.clear();
        }
        labels.insert(key, label.clone());
    });
    label
}
