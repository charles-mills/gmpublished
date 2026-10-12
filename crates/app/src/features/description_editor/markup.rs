//! Pure BBCode edit planning for the toolbar: given an action and the
//! current selection, produce the text to insert and where the caret lands.
//! Kept free of widget types so every toolbar behavior is testable as plain
//! string manipulation; `update` replays a plan through the editor content.

use unicode_segmentation::UnicodeSegmentation;

use crate::widgets::bbcode::heading_index;

/// A formatting operation the toolbar or a shortcut can request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolbarAction {
    Bold,
    Italic,
    Underline,
    Strikethrough,
    Heading(u8),
    BulletList,
    OrderedList,
    Quote,
    Code,
    Spoiler,
    Link,
    Image,
    Youtube,
    Table,
    HorizontalRule,
}

/// The text to paste over the current selection, and how many grapheme
/// clusters to step the caret back afterwards so it lands where typing
/// continues naturally (inside the empty tag pair, on a placeholder value,
/// …). Grapheme clusters, not `char`s: the plan is replayed as cursor
/// motions, and the editor's cursor moves one cluster at a time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct EditPlan {
    pub insert: String,
    pub caret_back: usize,
}

impl EditPlan {
    fn new(insert: impl Into<String>, caret_back: usize) -> Self {
        Self {
            insert: insert.into(),
            caret_back,
        }
    }

    /// Selection wrapped in a tag pair; caret ends after the closing tag.
    /// Without a selection the pair is empty and the caret sits inside it.
    fn wrap(name: &str, selection: Option<&str>) -> Self {
        let closing = format!("[/{name}]");
        selection.map_or_else(
            || Self::new(format!("[{name}]{closing}"), closing.chars().count()),
            |selection| Self::new(format!("[{name}]{selection}{closing}"), 0),
        )
    }
}

pub(super) fn plan(action: ToolbarAction, selection: Option<&str>) -> EditPlan {
    let selection = selection.filter(|selection| !selection.is_empty());
    match action {
        ToolbarAction::Bold => EditPlan::wrap("b", selection),
        ToolbarAction::Italic => EditPlan::wrap("i", selection),
        ToolbarAction::Underline => EditPlan::wrap("u", selection),
        ToolbarAction::Strikethrough => EditPlan::wrap("strike", selection),
        ToolbarAction::Heading(level) => {
            EditPlan::wrap(["h1", "h2", "h3"][heading_index(level)], selection)
        }
        ToolbarAction::Quote => EditPlan::wrap("quote", selection),
        ToolbarAction::Code => EditPlan::wrap("code", selection),
        ToolbarAction::Spoiler => EditPlan::wrap("spoiler", selection),
        ToolbarAction::BulletList => list_plan("list", selection),
        ToolbarAction::OrderedList => list_plan("olist", selection),
        // The caret lands in the target slot: `[url=|]text[/url]`.
        ToolbarAction::Link => selection.map_or_else(
            || EditPlan::new("[url=][/url]", "][/url]".chars().count()),
            |selection| {
                EditPlan::new(
                    format!("[url=]{selection}[/url]"),
                    selection.graphemes(true).count() + "][/url]".chars().count(),
                )
            },
        ),
        ToolbarAction::Image => selection.map_or_else(
            || EditPlan::new("[img][/img]", "[/img]".chars().count()),
            |selection| EditPlan::new(format!("[img]{selection}[/img]"), 0),
        ),
        ToolbarAction::Youtube => EditPlan::new(
            "[previewyoutube=][/previewyoutube]",
            "][/previewyoutube]".chars().count(),
        ),
        ToolbarAction::Table => {
            let table = "[table]\n[tr][th][/th][/tr]\n[tr][td][/td][/tr]\n[/table]";
            // Caret inside the first header cell.
            let after_caret = "[/th][/tr]\n[tr][td][/td][/tr]\n[/table]";
            EditPlan::new(table, after_caret.chars().count())
        }
        ToolbarAction::HorizontalRule => EditPlan::new("[hr]\n", 0),
    }
}

/// A selection becomes one item per non-empty line; no selection yields an
/// empty first item with the caret on it.
fn list_plan(name: &str, selection: Option<&str>) -> EditPlan {
    selection.map_or_else(
        || {
            let closing = format!("\n[/{name}]");
            EditPlan::new(format!("[{name}]\n[*] {closing}"), closing.chars().count())
        },
        |selection| {
            let items = selection
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| format!("[*] {}", line.trim()))
                .collect::<Vec<_>>()
                .join("\n");
            EditPlan::new(format!("[{name}]\n{items}\n[/{name}]"), 0)
        },
    )
}

/// What pressing Enter should do given the caret's current line: continue a
/// `[*]` list with a fresh item, dissolve the marker the user just abandoned,
/// or fall through to a plain newline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum EnterBehavior {
    /// Insert a newline and the next `[*] ` marker.
    ContinueList,
    /// The line is a bare marker: clear it instead of adding another.
    ClearMarker,
    Plain,
}

pub(super) fn enter_behavior(current_line: &str, caret: usize) -> EnterBehavior {
    let trimmed = current_line.trim_start();
    let Some(rest) = trimmed.strip_prefix("[*]") else {
        return EnterBehavior::Plain;
    };
    // List behavior only applies with the caret inside the item's content;
    // splitting the line at or inside the marker is a plain newline, so the
    // marker is never duplicated onto the freshly created line.
    let content_start =
        current_line.len() - trimmed.len() + "[*]".len() + usize::from(rest.starts_with(' '));
    if caret < content_start {
        return EnterBehavior::Plain;
    }
    if rest.trim().is_empty() {
        EnterBehavior::ClearMarker
    } else {
        EnterBehavior::ContinueList
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapping_a_selection_keeps_the_caret_after_the_closing_tag() {
        let plan = plan(ToolbarAction::Bold, Some("brave"));
        assert_eq!(plan.insert, "[b]brave[/b]");
        assert_eq!(plan.caret_back, 0);
    }

    #[test]
    fn wrapping_nothing_parks_the_caret_inside_the_pair() {
        let plan = plan(ToolbarAction::Strikethrough, None);
        assert_eq!(plan.insert, "[strike][/strike]");
        assert_eq!(plan.caret_back, "[/strike]".len());
    }

    #[test]
    fn link_plans_put_the_caret_in_the_target_slot() {
        let with_text = plan(ToolbarAction::Link, Some("the wiki"));
        assert_eq!(with_text.insert, "[url=]the wiki[/url]");
        assert_eq!(
            &with_text.insert[with_text.insert.chars().count() - with_text.caret_back..],
            "]the wiki[/url]"
        );

        let empty = plan(ToolbarAction::Link, None);
        assert_eq!(empty.insert, "[url=][/url]");
        assert_eq!(empty.caret_back, "][/url]".len());
    }

    #[test]
    fn selected_lines_become_list_items() {
        let plan = plan(ToolbarAction::BulletList, Some("one\n\n  two  \nthree"));
        assert_eq!(plan.insert, "[list]\n[*] one\n[*] two\n[*] three\n[/list]");
        assert_eq!(plan.caret_back, 0);
    }

    #[test]
    fn empty_list_plan_leaves_the_caret_on_the_first_item() {
        let plan = plan(ToolbarAction::OrderedList, None);
        assert_eq!(plan.insert, "[olist]\n[*] \n[/olist]");
        assert_eq!(plan.caret_back, "\n[/olist]".len());
    }

    fn enter_at_end(line: &str) -> EnterBehavior {
        enter_behavior(line, line.len())
    }

    #[test]
    fn enter_continues_populated_items_and_clears_bare_markers() {
        assert_eq!(enter_at_end("[*] filled"), EnterBehavior::ContinueList);
        assert_eq!(enter_at_end("  [*] filled"), EnterBehavior::ContinueList);
        assert_eq!(enter_at_end("[*] "), EnterBehavior::ClearMarker);
        assert_eq!(enter_at_end("[*]"), EnterBehavior::ClearMarker);
        assert_eq!(enter_at_end("plain prose"), EnterBehavior::Plain);
        assert_eq!(enter_at_end(""), EnterBehavior::Plain);
    }

    /// Splitting a line at or inside its marker must not clone the marker
    /// onto the new line: `⏎[*] item` would otherwise become `[*] [*] item`.
    #[test]
    fn enter_before_the_item_content_is_a_plain_newline() {
        assert_eq!(enter_behavior("[*] item", 0), EnterBehavior::Plain);
        assert_eq!(enter_behavior("[*] item", 1), EnterBehavior::Plain);
        assert_eq!(enter_behavior("[*] item", 3), EnterBehavior::Plain);
        assert_eq!(enter_behavior("[*] item", 4), EnterBehavior::ContinueList);
        assert_eq!(enter_behavior("  [*] item", 5), EnterBehavior::Plain);
        assert_eq!(enter_behavior("  [*] item", 6), EnterBehavior::ContinueList);
        // Splitting an item's content makes two items out of it.
        assert_eq!(enter_behavior("[*] item", 6), EnterBehavior::ContinueList);
        assert_eq!(enter_behavior("[*] ", 0), EnterBehavior::Plain);
    }

    /// `caret_back` counts grapheme clusters because it replays as cursor
    /// motions: a flag emoji is four `char`s but a single cursor step.
    #[test]
    fn link_caret_counts_selection_graphemes_not_chars() {
        let plan = plan(ToolbarAction::Link, Some("🇬🇧"));
        assert_eq!(plan.insert, "[url=]🇬🇧[/url]");
        assert_eq!(plan.caret_back, 1 + "][/url]".len());
    }

    #[test]
    fn heading_levels_clamp_to_the_steam_range() {
        assert_eq!(plan(ToolbarAction::Heading(1), None).insert, "[h1][/h1]");
        assert_eq!(plan(ToolbarAction::Heading(3), None).insert, "[h3][/h3]");
        assert_eq!(plan(ToolbarAction::Heading(9), None).insert, "[h3][/h3]");
    }
}
