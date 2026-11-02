//! IME composition ownership and cell-based layout, separate from PTY input.

use unicode_width::UnicodeWidthChar;
use winit::event::Ime;

use crate::term::PaneId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preedit {
    pub text: String,
    /// UTF-8 byte offsets, as supplied by winit. The end is exclusive.
    pub cursor: Option<(usize, usize)>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct PreeditGlyph {
    pub character: char,
    /// Absolute column in the terminal grid. Combining marks share their base column.
    pub column: usize,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PreeditLayout {
    pub start_column: usize,
    pub width: usize,
    pub glyphs: Vec<PreeditGlyph>,
    pub cursor_column: Option<usize>,
    /// Absolute selected columns, with an exclusive end.
    pub selection: Option<(usize, usize)>,
    pub candidate_column: usize,
    pub candidate_width: usize,
}

impl Preedit {
    pub fn new(text: String, cursor: Option<(usize, usize)>) -> Self {
        let cursor = cursor.map(|(start, end)| {
            let start = floor_char_boundary(&text, start);
            let end = floor_char_boundary(&text, end);
            (start.min(end), start.max(end))
        });
        Self { text, cursor }
    }

    /// Keep a single composition row inside the pane, scrolling to the selected
    /// clause or caret when necessary. A short composition may shift left near
    /// the right edge; a long one can use the full row. Wide glyphs are never
    /// drawn partly outside the row.
    pub fn layout(&self, columns: usize, preferred_start_column: usize) -> PreeditLayout {
        if columns == 0 || self.text.is_empty() {
            return PreeditLayout::default();
        }

        let text_width = cell_width(&self.text);
        let width = text_width.saturating_add(1).min(columns).max(1);
        let start_column = preferred_start_column.min(columns - width);
        let cursor = self.cursor.map(|(start, end)| {
            (
                cell_width(&self.text[..floor_char_boundary(&self.text, start)]),
                cell_width(&self.text[..floor_char_boundary(&self.text, end)]),
            )
        });
        let anchor = cursor.map_or(text_width, |(start, _)| start);

        // Move the left boundary forward to a glyph boundary. This leaves the
        // caret visible without cutting a Japanese wide character in half.
        let requested_scroll = anchor.saturating_sub(width - 1);
        let mut column = 0usize;
        let mut scroll = text_width;
        let mut last_base = None;
        for character in self.text.chars() {
            let character_width = glyph_width(character);
            if character_width > 0 {
                last_base = Some((column, character_width));
            }
            if character_width > 0 && column >= requested_scroll {
                scroll = column;
                break;
            }
            column = column.saturating_add(character_width);
        }
        if requested_scroll == 0 {
            scroll = 0;
        } else if scroll == text_width {
            // In a two-cell pane the final wide glyph and a spare caret cell
            // cannot both fit. Keep the glyph visible and clamp the thin caret
            // to the row's final cell instead of displaying an empty overlay.
            if let Some((column, glyph_width)) = last_base
                && glyph_width <= width {
                scroll = column;
            }
        }

        let mut glyphs = Vec::new();
        let mut column = 0usize;
        let mut base = None;
        for character in self.text.chars() {
            if character.is_control() {
                continue;
            }
            let character_width = glyph_width(character);
            let (glyph_column, glyph_extent) = if character_width == 0 {
                // A combining mark belongs to its preceding base even when a
                // horizontal viewport hides that base.
                let Some(base) = base else { continue };
                base
            } else {
                let current = (column, character_width);
                base = Some(current);
                column = column.saturating_add(character_width);
                current
            };
            if glyph_column >= scroll
                && glyph_column.saturating_add(glyph_extent) <= scroll.saturating_add(width)
            {
                glyphs.push(PreeditGlyph {
                    character,
                    column: start_column + glyph_column - scroll,
                });
            }
        }

        let visible_column = |column: usize| {
            start_column + column.saturating_sub(scroll).min(width - 1)
        };
        let cursor_column = cursor.map(|(start, _)| visible_column(start));
        let selection = cursor.and_then(|(start, end)| {
            let start = start.saturating_sub(scroll).min(width);
            let end = end.saturating_sub(scroll).min(width);
            (start < end).then_some((start_column + start, start_column + end))
        });
        let candidate_column = visible_column(anchor);
        let candidate_width = selection
            .map_or(1, |(start, end)| end - start)
            .min(start_column + width - candidate_column);

        PreeditLayout {
            start_column,
            width,
            glyphs,
            cursor_column,
            selection,
            candidate_column,
            candidate_width,
        }
    }
}

fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn glyph_width(character: char) -> usize {
    UnicodeWidthChar::width(character).unwrap_or(0)
}

fn cell_width(text: &str) -> usize {
    text.chars().map(glyph_width).sum()
}

/// A composition belongs to the pane where it began. Empty preedit updates
/// clear the overlay but retain ownership, since winit sends one before Commit.
#[derive(Default, Debug)]
pub struct ImeState {
    target: Option<PaneId>,
    owner: Option<PaneId>,
    preedit: Option<Preedit>,
}

impl ImeState {
    /// Return true when native IME composition must also be cancelled/reset.
    pub fn set_target(&mut self, target: Option<PaneId>) -> bool {
        if self.target == target {
            return false;
        }
        self.target = target;
        self.reset();
        true
    }

    pub fn preedit(&self) -> Option<&Preedit> {
        self.preedit.as_ref()
    }

    /// Return committed text only when its originating pane is still targeted.
    /// Ordinary KeyboardInput remains independent: winit already filters keys
    /// handled by macOS IME and forwards application commands intentionally.
    pub fn update(&mut self, event: Ime) -> Option<String> {
        match event {
            Ime::Enabled => {
                self.owner = self.target;
            }
            Ime::Disabled => self.reset(),
            Ime::Preedit(text, cursor) => {
                if text.is_empty() {
                    self.preedit = None;
                } else if let Some(target) = self.target {
                    self.owner = Some(target);
                    self.preedit = Some(Preedit::new(text, cursor));
                }
            }
            Ime::Commit(text) => {
                let valid = self.owner.is_some() && self.owner == self.target;
                self.reset();
                return (valid && !text.is_empty()).then_some(text);
            }
        }
        None
    }

    /// Cancel composition, preserving the currently eligible input pane.
    pub fn reset(&mut self) {
        self.owner = None;
        self.preedit = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn japanese_selection_uses_byte_offsets_and_two_cell_glyphs() {
        let preedit = Preedit::new("a日本b".into(), Some((1, 7)));
        let layout = preedit.layout(20, 2);
        assert_eq!(layout.selection, Some((3, 7)));
        assert_eq!(layout.cursor_column, Some(3));
        assert_eq!(layout.candidate_column, 3);
        assert_eq!(layout.candidate_width, 4);
        assert_eq!(layout.glyphs.iter().map(|g| g.column).collect::<Vec<_>>(), [2, 3, 5, 7]);
    }

    #[test]
    fn combining_mark_stays_on_its_base() {
        let preedit = Preedit::new("e\u{301}日".into(), Some((3, 3)));
        let layout = preedit.layout(10, 0);
        assert_eq!(layout.cursor_column, Some(1));
        assert_eq!(layout.glyphs.iter().map(|g| g.column).collect::<Vec<_>>(), [0, 0, 1]);
    }

    #[test]
    fn right_edge_shifts_composition_inside_pane() {
        let layout = Preedit::new("日本".into(), Some((6, 6))).layout(10, 9);
        assert_eq!(layout.start_column, 5);
        assert_eq!(layout.cursor_column, Some(9));
        assert_eq!(layout.glyphs.iter().map(|g| g.column).collect::<Vec<_>>(), [5, 7]);
    }

    #[test]
    fn long_composition_scrolls_to_caret_without_splitting_wide_characters() {
        let text = "日本語入力文字列";
        let layout = Preedit::new(text.into(), Some((text.len(), text.len()))).layout(6, 5);
        assert_eq!(layout.start_column, 0);
        assert_eq!(layout.cursor_column, Some(4));
        assert_eq!(layout.glyphs.iter().map(|g| g.character).collect::<String>(), "字列");
        assert!(layout.glyphs.iter().all(|g| g.column + glyph_width(g.character) <= 6));
    }

    #[test]
    fn hidden_caret_and_invalid_boundaries_are_safe() {
        let preedit = Preedit::new("日本".into(), Some((999, 1)));
        assert_eq!(preedit.cursor, Some((0, 6)));
        assert_eq!(Preedit::new("日".into(), None).layout(2, 0).cursor_column, None);
        assert_eq!(preedit.layout(0, 0), PreeditLayout::default());
        let narrow = preedit.layout(1, 0);
        assert!(narrow.glyphs.is_empty());
        assert_eq!(narrow.candidate_width, 1);
    }

    #[test]
    fn two_cell_pane_keeps_the_final_wide_glyph_visible() {
        let layout = Preedit::new("日本".into(), Some((6, 6))).layout(2, 1);
        assert_eq!(layout.glyphs, [PreeditGlyph { character: '本', column: 0 }]);
        assert_eq!(layout.cursor_column, Some(1));
        assert_eq!(layout.candidate_width, 1);
    }

    #[test]
    fn empty_preedit_before_commit_keeps_ownership() {
        let mut state = ImeState::default();
        state.set_target(Some(7));
        state.update(Ime::Enabled);
        state.update(Ime::Preedit("にほん".into(), Some((9, 9))));
        state.update(Ime::Preedit(String::new(), None));
        assert!(state.preedit().is_none());
        assert_eq!(state.update(Ime::Commit("日本".into())), Some("日本".into()));
        assert_eq!(state.update(Ime::Commit("日本".into())), None);
    }

    #[test]
    fn focus_change_drops_old_panes_commit() {
        let mut state = ImeState::default();
        state.set_target(Some(1));
        state.update(Ime::Preedit("あ".into(), Some((3, 3))));
        assert!(state.set_target(Some(2)));
        state.update(Ime::Preedit(String::new(), None));
        assert_eq!(state.update(Ime::Commit("亜".into())), None);
        assert!(state.preedit().is_none());
        assert!(!state.set_target(Some(2)));
    }

    #[test]
    fn preedit_can_begin_without_enabled_event_but_not_without_a_target() {
        let mut state = ImeState::default();
        state.update(Ime::Preedit("あ".into(), Some((3, 3))));
        assert_eq!(state.update(Ime::Commit("亜".into())), None);
        state.set_target(Some(2));
        state.update(Ime::Preedit("あ".into(), Some((3, 3))));
        assert_eq!(state.update(Ime::Commit("亜".into())), Some("亜".into()));
        state.update(Ime::Enabled);
        state.update(Ime::Disabled);
        assert_eq!(state.update(Ime::Commit("亜".into())), None);
    }
}
