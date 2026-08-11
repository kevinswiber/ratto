use std::io::Read;

use anyhow::Context;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthStr;

use crate::cli::FilterArgs;
use crate::color::ColorProfile;
use crate::core::duration::parse_interval;
use crate::exit::{AppError, AppResult};
use crate::theme::Palette;
use crate::ui::echo::EchoSnapshot;
use crate::ui::filter::FilterState;
use crate::ui::key::Key;
use crate::ui::loop_::{Outcome, UiApp, UiMode, run_ui_mode};

struct FilterApp {
    state: FilterState,
    prompt: String,
    placeholder: String,
    indicator: String,
    selected_prefix: String,
    unselected_prefix: String,
    multi: bool,
    /// `--no-strict`, inverted at construction. It lives here rather
    /// than in the run because the closing row is spoken from inside the
    /// driver, where only the app is reachable — and a second copy of
    /// the print rule is exactly what would disagree on the one arm that
    /// matters.
    strict: bool,
    header: Option<String>,
    palette: Palette,
}

/// The one phrase for an empty match set. The opening block and a
/// settled query row both reach for it, and a reader should meet it in
/// one spelling.
const NO_MATCHES: &str = "no matches";

impl FilterApp {
    /// The query as it now stands, what it matched, and where the reader
    /// landed — the resting state, not the edit that produced it, so a
    /// row held through a burst is still true when it is written.
    fn query_row(&self) -> String {
        let query = &self.state.query.value;
        match self.state.matches.len() {
            0 => format!("{query}, {NO_MATCHES}"),
            // Singular, because the row is read aloud.
            1 => format!("{query}, 1 match, {}", self.cursor_item()),
            n => format!("{query}, {n} matches, {}", self.cursor_item()),
        }
    }

    /// Exactly what this run puts on stdout, in the order it prints
    /// them: the marked items, else the item under the cursor, else —
    /// when the reader asked for it — the query they typed. Matching
    /// gum, a strict run prints nothing when nothing matched and a
    /// non-strict one returns the query itself.
    ///
    /// One function, because the answer is read twice: once to print it
    /// and once to say it. Derived twice, the two would agree on every
    /// ordinary run and disagree on exactly the run where the reader
    /// most needs to be told what happened.
    fn printed(&self) -> Vec<String> {
        let results = self.state.results();
        if !results.is_empty() {
            return results;
        }
        if !self.strict && !self.state.query.value.is_empty() {
            return vec![self.state.query.value.clone()];
        }
        Vec::new()
    }

    /// The mark that CHANGED, named by what the reader did. A single
    /// selection clears the old mark as it sets the new one, so two
    /// entries move at once — report the one that was SET, because that
    /// is the gesture; the clearing is what a single selection means,
    /// not a second event.
    fn mark_row(&self, before: &EchoSnapshot) -> Option<String> {
        let now = &self.state.selected;
        let set = before.marked.iter().zip(now).position(|(b, n)| !b && *n);
        if let Some(idx) = set {
            return Some(format!("selected {}", self.state.items[idx]));
        }
        let clear = before.marked.iter().zip(now).position(|(b, n)| *b && !n);
        clear.map(|idx| format!("deselected {}", self.state.items[idx]))
    }

    /// The item under the cursor. Total by construction: refreshing a
    /// query resets the cursor while the match list may be empty, and a
    /// partial function on the loop thread is not worth the saving.
    fn cursor_item(&self) -> String {
        self.state
            .matches
            .get(self.state.cursor)
            .map(|m| self.state.items[m.index].clone())
            .unwrap_or_default()
    }
}

impl UiApp for FilterApp {
    fn on_key(&mut self, key: Key) -> Outcome {
        self.state.on_key(key)
    }

    fn render(&self, _area: Rect, buf: &mut Buffer) {
        // The prompt is a prompt (accent); the marker and cursor row are a
        // selection. Same values by construction, distinct tokens by role.
        let accent = Style::default().fg(self.palette.accent);
        let selection = Style::default().fg(self.palette.selection);
        let mut y = 0;
        if let Some(header) = &self.header {
            buf.set_string(0, y, header, Style::default().add_modifier(Modifier::BOLD));
            y += 1;
        }
        buf.set_string(0, y, &self.prompt, accent);
        // Cell width, not char count — same rule as the input prompt.
        let qx = self.prompt.as_str().width() as u16;
        // The query scrolls in its own window, same as `rat input`.
        let qtext = self.state.query.visible(&self.placeholder);
        let qstyle = if self.state.query.value.is_empty() {
            Style::default()
                .add_modifier(Modifier::DIM)
                .fg(self.palette.placeholder)
        } else {
            Style::default()
        };
        buf.set_string(qx, y, &qtext, qstyle);
        y += 1;

        let visible = usize::from(self.state.height);
        let window = self
            .state
            .matches
            .iter()
            .enumerate()
            .skip(self.state.offset)
            .take(visible);
        for (match_idx, m) in window {
            let at_cursor = match_idx == self.state.cursor;
            let mut x = 0u16;
            let marker = if at_cursor { &self.indicator } else { "  " };
            buf.set_string(x, y, marker, selection);
            // x is a cell cursor: a wide glyph advances it by two.
            x += marker.width() as u16;
            if self.multi {
                let prefix = if self.state.selected[m.index] {
                    &self.selected_prefix
                } else {
                    &self.unselected_prefix
                };
                buf.set_string(x, y, prefix, Style::default());
                x += prefix.as_str().width() as u16;
            }
            let item = &self.state.items[m.index];
            let base = if at_cursor {
                selection
            } else {
                Style::default()
            };
            buf.set_string(x, y, item, base);
            // Highlight the matched characters. `positions` are char
            // indices into the item; the buffer takes cells, so the
            // offset is the width of everything before the match — a
            // char index would repaint an earlier cell.
            for &pos in &m.positions {
                if let Some(c) = item.chars().nth(pos as usize) {
                    let before: String = item.chars().take(pos as usize).collect();
                    buf.set_string(
                        x + before.width() as u16,
                        y,
                        c.to_string(),
                        base.add_modifier(Modifier::BOLD).fg(self.palette.r#match),
                    );
                }
            }
            y += 1;
        }
    }

    fn height(&self, _term: (u16, u16)) -> u16 {
        let rows = (self.state.matches.len() as u16).min(self.state.height);
        1 + rows + u16::from(self.header.is_some())
    }

    fn cursor_pos(&self) -> Option<(u16, u16)> {
        // Cells, not chars, and measured inside the query's window.
        let row = u16::from(self.header.is_some());
        let col = self.prompt.as_str().width() + self.state.query.caret_col();
        Some((col as u16, row))
    }

    fn echo_snapshot(&self) -> EchoSnapshot {
        EchoSnapshot {
            cursor: self.state.cursor,
            marked: self.state.selected.clone(),
            query: self.state.query.value.clone(),
        }
    }

    fn speak(&self, before: &EchoSnapshot) -> Option<String> {
        // The query is the coarsest thing a key can move, and moving it
        // resets the cursor, so a query change subsumes the cursor row
        // it would otherwise have produced.
        if before.query != self.state.query.value {
            return Some(self.query_row());
        }
        if let Some(row) = self.mark_row(before) {
            return Some(row);
        }
        if before.cursor != self.state.cursor {
            // The match's name and nothing else: the count and the query
            // were said when the query settled.
            return Some(self.cursor_item());
        }
        None
    }

    fn speak_orientation(&self) -> Vec<String> {
        if self.state.matches.is_empty() {
            return vec![NO_MATCHES.to_string()];
        }
        // The count is MATCHES: the cursor indexes them, so counting
        // candidates would be a number the cursor does not mean.
        vec![format!(
            "{} {} of {}",
            self.cursor_item(),
            self.state.cursor + 1,
            self.state.matches.len()
        )]
    }

    fn speak_selection(&self) -> Vec<String> {
        let marked = self.state.selected.iter().filter(|m| **m).count();
        if marked == 0 {
            return vec!["nothing selected".to_string()];
        }
        // The reducer's own list of what is tagged, which is what the
        // printed line names too.
        vec![format!(
            "{marked} selected, {}",
            self.state.results().join(", ")
        )]
    }

    fn speak_closing(&self, outcome: &AppResult) -> Vec<String> {
        match outcome {
            Ok(()) => {
                let printed = self.printed();
                if printed.is_empty() {
                    vec!["chose nothing".to_string()]
                } else {
                    // The vocabulary's comma, never the output
                    // delimiter — whose default is a newline, which
                    // would let a result forge a transcript row.
                    vec![format!("chose {}", printed.join(", "))]
                }
            }
            // Both ways of declining read the same: the reader stopped.
            Err(AppError::NoSelection) | Err(AppError::Aborted) => vec!["cancelled".to_string()],
            // The one ending with no keystroke behind it, so the only
            // place the reader can learn why the session stopped.
            Err(AppError::Timeout(_)) => vec!["timed out".to_string()],
            // Not a choice the reader made, and stderr has already said
            // why. No catch-all arm — the next variant added should have
            // to answer this question.
            Err(AppError::Fail(_)) | Err(AppError::Child(_)) => Vec::new(),
        }
    }

    fn speak_opening(&self) -> Vec<String> {
        // The MATCHES, not the raw candidate list: with a seeded query
        // the two differ, and the block must describe what the reader is
        // actually looking at.
        let listed: Vec<String> = self
            .state
            .matches
            .iter()
            .map(|m| self.state.items[m.index].clone())
            .collect();
        let verbs = if self.multi {
            "type to filter, up and down move, tab selects, enter confirms, escape cancels"
        } else {
            "type to filter, up and down move, enter chooses, escape cancels"
        };
        let keys = format!("{verbs}{}", crate::ui::echo::on_demand_clause(self.multi));
        let position = if listed.is_empty() {
            // The same phrase a settled query uses, so a reader meets it
            // once. There is no first of nothing.
            NO_MATCHES.to_string()
        } else {
            format!("1 of {}", listed.len())
        };
        crate::ui::echo::opening_block(
            // Absent becomes the empty string, which the builder drops —
            // never a blank leading row. The heading is `--header`; the
            // prompt is a painted prefix and has nothing to say here.
            self.header.as_deref().unwrap_or(""),
            &listed,
            &position,
            &keys,
        )
    }

    fn prepare(&mut self, term: (u16, u16)) {
        let avail = usize::from(term.0).saturating_sub(self.prompt.as_str().width());
        self.state.query.follow(avail);
    }
}

pub fn run(args: FilterArgs, profile: ColorProfile, palette: Palette, mode: UiMode) -> AppResult {
    let mut stdin = String::new();
    std::io::stdin()
        .read_to_string(&mut stdin)
        .context("reading stdin")?;
    let delim = if args.input_delimiter == "\\n" {
        "\n"
    } else {
        &args.input_delimiter
    };
    let items: Vec<String> = stdin
        .trim_end_matches('\n')
        .split(delim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    if args.select_if_one && items.len() == 1 {
        println!("{}", items[0]);
        return Ok(());
    }

    let out_delim = if args.output_delimiter == "\\n" {
        "\n"
    } else {
        &args.output_delimiter
    };
    let limit = if args.no_limit {
        None
    } else {
        Some(args.limit)
    };
    let fuzzy = !args.no_fuzzy;
    let sort = !args.no_fuzzy_sort;
    let state = FilterState::new(items, limit, args.height, fuzzy, sort, args.value.clone());
    let mut app = FilterApp {
        state,
        prompt: args.prompt.clone(),
        placeholder: args.placeholder.clone(),
        indicator: args.indicator.clone(),
        selected_prefix: args.selected_prefix.clone(),
        unselected_prefix: args.unselected_prefix.clone(),
        multi: limit != Some(1),
        strict: !args.no_strict,
        header: args.header.clone(),
        palette,
    };
    let timeout = args.timeout.as_deref().map(parse_interval).transpose()?;
    run_ui_mode(&mut app, profile, timeout, mode)?;

    let printed = app.printed();
    if !printed.is_empty() {
        println!("{}", printed.join(out_delim));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;
    use crate::theme::{Appearance, AppearanceSource};

    #[test]
    fn the_on_demand_rows_count_matches_rather_than_candidates() {
        // Three candidates, a query narrowing to two: an implementation
        // that counted candidates says `1 of 3`, and the cursor indexes
        // matches, so that would be a number the cursor does not mean.
        let dark = Palette::builtin(Appearance::Dark, AppearanceSource::Default);
        let mut a = app(dark);
        a.state = FilterState::new(
            vec!["alpha".into(), "beta".into(), "apricot".into()],
            Some(1),
            5,
            true,
            true,
            "ap".into(),
        );
        assert_eq!(a.speak_orientation(), ["apricot 1 of 2"]);
        assert_eq!(a.speak_selection(), ["nothing selected"]);

        a.state.on_key(Key::Tab);
        assert_eq!(a.speak_selection(), ["1 selected, apricot"]);

        let mut none = app(dark);
        none.state = FilterState::new(
            vec!["alpha".into(), "beta".into()],
            Some(1),
            5,
            true,
            true,
            "zz".into(),
        );
        assert_eq!(none.speak_orientation(), ["no matches"]);
    }

    #[test]
    fn every_way_the_session_can_end_answers_for_itself() {
        let dark = Palette::builtin(Appearance::Dark, AppearanceSource::Default);
        // The seeded query matches one item, so the cursor is on it.
        assert_eq!(app(dark).speak_closing(&Ok(())), ["chose alpha"]);

        // Nothing matched and nothing printed: a strict run says so.
        // The query is seeded at construction, which is the only way to
        // reach a matched-nothing state without reaching into the
        // reducer.
        let mut empty = app(dark);
        empty.state = FilterState::new(
            vec!["alpha".into(), "beta".into()],
            Some(1),
            5,
            true,
            true,
            "zz".into(),
        );
        assert_eq!(empty.speak_closing(&Ok(())), ["chose nothing"]);

        // The same state one flag apart: a non-strict run prints the
        // query, so the row must name it. This is the input where a row
        // derived from the selection alone disagrees with stdout.
        let mut loose = app(dark);
        loose.strict = false;
        loose.state = FilterState::new(
            vec!["alpha".into(), "beta".into()],
            Some(1),
            5,
            true,
            true,
            "zz".into(),
        );
        assert_eq!(loose.speak_closing(&Ok(())), ["chose zz"]);
        assert_eq!(loose.printed(), ["zz"]);

        let a = app(dark);
        assert_eq!(a.speak_closing(&Err(AppError::NoSelection)), ["cancelled"]);
        // Raw mode clears the terminal's signal handling, so this
        // arrives as a keystroke and the row still gets written.
        assert_eq!(a.speak_closing(&Err(AppError::Aborted)), ["cancelled"]);
        assert_eq!(
            a.speak_closing(&Err(AppError::Timeout(None))),
            ["timed out"]
        );
        assert_eq!(
            a.speak_closing(&Err(AppError::Timeout(Some(anyhow::anyhow!("waiting"))))),
            ["timed out"]
        );
        assert!(
            a.speak_closing(&Err(anyhow::anyhow!("boom").into()))
                .is_empty()
        );
        assert!(a.speak_closing(&Err(AppError::Child(7))).is_empty());
    }

    fn app(palette: Palette) -> FilterApp {
        FilterApp {
            state: FilterState::new(
                vec!["alpha".into(), "beta".into()],
                Some(1),
                5,
                true,
                true,
                "a".into(),
            ),
            prompt: "> ".into(),
            placeholder: "filter".into(),
            indicator: "> ".into(),
            selected_prefix: "[x] ".into(),
            unselected_prefix: "[ ] ".into(),
            multi: false,
            strict: true,
            header: None,
            palette,
        }
    }

    fn prompt_and_highlight_fg(palette: Palette) -> (Color, Color) {
        let app = app(palette);
        let area = Rect::new(0, 0, 40, 6);
        let mut buf = Buffer::empty(area);
        app.render(area, &mut buf);
        let prompt = buf.cell((0, 0)).expect("prompt is painted").fg;
        let highlight = (0u16..40)
            .filter_map(|x| buf.cell((x, 1)))
            .find(|cell| cell.modifier.contains(Modifier::BOLD))
            .expect("a match highlight is painted")
            .fg;
        (prompt, highlight)
    }

    fn rendered(palette: Palette) -> Vec<String> {
        let app = app(palette);
        let area = Rect::new(0, 0, 40, 6);
        let mut buf = Buffer::empty(area);
        app.render(area, &mut buf);
        crate::term::buffer_ansi::buffer_to_lines(&buf, ColorProfile::Ansi256)
    }

    #[test]
    fn the_rendered_filter_frame_is_pinned() {
        // Byte-identity golden: captured on v0.5.0 render code, before the
        // selection/match/placeholder rewire. Note the marker, cursor-row
        // base, and match highlight all read accent-valued styles here.
        let dark = rendered(Palette::builtin(
            Appearance::Dark,
            AppearanceSource::Default,
        ));
        assert_eq!(
            dark,
            [
                "\u{1b}[38;5;212m> \u{1b}[0ma",
                "\u{1b}[38;5;212m> \u{1b}[0m\u{1b}[1;38;5;212ma\u{1b}[0m\u{1b}[38;5;212mlpha\u{1b}[0m",
                "\u{1b}[38;5;212m  \u{1b}[0mbet\u{1b}[1;38;5;212ma\u{1b}[0m",
                "",
                "",
                ""
            ]
        );
        let light = rendered(Palette::builtin(
            Appearance::Light,
            AppearanceSource::Default,
        ));
        assert_eq!(
            light,
            [
                "\u{1b}[38;5;129m> \u{1b}[0ma",
                "\u{1b}[38;5;129m> \u{1b}[0m\u{1b}[1;38;5;129ma\u{1b}[0m\u{1b}[38;5;129mlpha\u{1b}[0m",
                "\u{1b}[38;5;129m  \u{1b}[0mbet\u{1b}[1;38;5;129ma\u{1b}[0m",
                "",
                "",
                ""
            ]
        );
    }

    #[test]
    fn the_marker_and_cursor_row_read_the_selection_token() {
        // Sentinel: selection/accent share a value by construction, so only
        // a diverging palette proves the render reads `selection`.
        let palette = Palette {
            selection: Color::Indexed(99),
            ..Palette::builtin(Appearance::Dark, AppearanceSource::Default)
        };
        let app = app(palette);
        let area = Rect::new(0, 0, 40, 6);
        let mut buf = Buffer::empty(area);
        app.render(area, &mut buf);
        assert_eq!(buf.cell((0, 1)).unwrap().fg, Color::Indexed(99), "marker");
        assert_eq!(
            buf.cell((3, 1)).unwrap().fg,
            Color::Indexed(99),
            "cursor-row item base"
        );
        // The prompt is a prompt, not a selection: it stays on accent.
        assert_eq!(buf.cell((0, 0)).unwrap().fg, Color::Indexed(212), "prompt");
    }

    #[test]
    fn a_match_highlight_lands_on_its_own_cell_past_a_wide_glyph() {
        // `positions` are CHAR indices; the buffer takes CELLS. Treating
        // one as the other repaints the match over an earlier cell — a
        // query of "o" against "🔥 hot" rewrote the 'h', so the row read
        // "🔥 oot".
        let app = FilterApp {
            state: FilterState::new(
                vec!["🔥 hot".into(), "日本 jp".into()],
                Some(1),
                5,
                true,
                true,
                "o".into(),
            ),
            prompt: "> ".into(),
            placeholder: "filter".into(),
            indicator: "> ".into(),
            selected_prefix: "[x] ".into(),
            unselected_prefix: "[ ] ".into(),
            multi: false,
            strict: true,
            header: None,
            palette: Palette::builtin(Appearance::Dark, AppearanceSource::Default),
        };
        let area = Rect::new(0, 0, 40, 6);
        let mut buf = Buffer::empty(area);
        app.render(area, &mut buf);
        // Row 1: indicator "> " (2 cells), then 🔥 (2), ' ', 'h', 'o', 't'.
        let line = &crate::term::buffer_ansi::buffer_to_lines(&buf, ColorProfile::Ascii)[1];
        assert!(
            line.contains("🔥 hot"),
            "the item text must survive its own highlight: {line:?}"
        );
        // The 'o' sits at column 2 + 2 + 1 + 1 = 6 and carries the match.
        let cell = buf.cell((6, 1)).expect("the matched cell is painted");
        assert_eq!(cell.symbol(), "o");
        assert!(cell.modifier.contains(Modifier::BOLD));
        // And the 'h' before it is untouched.
        assert_eq!(buf.cell((5, 1)).unwrap().symbol(), "h");
    }

    #[test]
    fn a_wide_indicator_or_prefix_advances_by_cells() {
        // The running x is a cell cursor, so a wide indicator must move
        // it two cells, not one char.
        let app = FilterApp {
            state: FilterState::new(vec!["ab".into()], None, 5, true, true, String::new()),
            prompt: "> ".into(),
            placeholder: "filter".into(),
            indicator: "日".into(),
            selected_prefix: "[x] ".into(),
            unselected_prefix: "[ ] ".into(),
            multi: true,
            strict: true,
            header: None,
            palette: Palette::builtin(Appearance::Dark, AppearanceSource::Default),
        };
        let area = Rect::new(0, 0, 40, 6);
        let mut buf = Buffer::empty(area);
        app.render(area, &mut buf);
        // 日 spans columns 0-1, so the "[ ] " prefix starts at 2 and the
        // item at 6.
        assert_eq!(buf.cell((2, 1)).unwrap().symbol(), "[");
        assert_eq!(buf.cell((6, 1)).unwrap().symbol(), "a");
    }

    #[test]
    fn the_match_highlight_reads_the_match_token() {
        let palette = Palette {
            r#match: Color::Indexed(98),
            ..Palette::builtin(Appearance::Dark, AppearanceSource::Default)
        };
        let app = app(palette);
        let area = Rect::new(0, 0, 40, 6);
        let mut buf = Buffer::empty(area);
        app.render(area, &mut buf);
        let cell = buf.cell((2, 1)).unwrap();
        assert_eq!(cell.fg, Color::Indexed(98));
        // The BOLD attribute is part of the highlight contract.
        assert!(cell.modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn the_empty_query_reads_the_placeholder_token() {
        let palette = Palette {
            placeholder: Color::Indexed(97),
            ..Palette::builtin(Appearance::Dark, AppearanceSource::Default)
        };
        let app = FilterApp {
            state: FilterState::new(
                vec!["alpha".into(), "beta".into()],
                Some(1),
                5,
                true,
                true,
                String::new(),
            ),
            prompt: "> ".into(),
            placeholder: "filter".into(),
            indicator: "> ".into(),
            selected_prefix: "[x] ".into(),
            unselected_prefix: "[ ] ".into(),
            multi: false,
            strict: true,
            header: None,
            palette,
        };
        let area = Rect::new(0, 0, 40, 6);
        let mut buf = Buffer::empty(area);
        app.render(area, &mut buf);
        let cell = buf.cell((2, 0)).unwrap();
        assert_eq!(cell.fg, Color::Indexed(97));
        // DIM is what keeps the default byte-identical (Reset emits nothing).
        assert!(cell.modifier.contains(Modifier::DIM));
    }

    #[test]
    fn the_cursor_pos_tracks_the_query_caret() {
        // Initial query "a" leaves the caret at its end: prompt width 2
        // plus 1, on the query row.
        let app = app(Palette::builtin(
            Appearance::Dark,
            AppearanceSource::Default,
        ));
        assert_eq!(app.cursor_pos(), Some((3, 0)));
    }

    #[test]
    fn the_cursor_pos_counts_display_cells_not_chars() {
        // A wide query char occupies two cells; the parked column follows
        // the cells, not the char count.
        let mut app = app(Palette::builtin(
            Appearance::Dark,
            AppearanceSource::Default,
        ));
        app.state.query.value = "日".to_string();
        app.state.query.cursor = 1;
        assert_eq!(app.cursor_pos(), Some((4, 0)));
    }

    #[test]
    fn the_prompt_and_match_highlight_take_their_palette_tokens() {
        let dark = Palette::builtin(Appearance::Dark, AppearanceSource::Default);
        let light = Palette::builtin(Appearance::Light, AppearanceSource::Default);
        let (dark_prompt, dark_highlight) = prompt_and_highlight_fg(dark);
        assert_eq!(dark_prompt, dark.accent);
        // The literal is the byte-identity pin; the field name is routing.
        assert_eq!(dark_prompt, Color::Indexed(212));
        assert_eq!(dark_highlight, dark.r#match);
        assert_eq!(dark_highlight, Color::Indexed(212));
        let (light_prompt, light_highlight) = prompt_and_highlight_fg(light);
        assert_eq!(light_prompt, light.accent);
        assert_eq!(light_highlight, light.r#match);
    }
}
