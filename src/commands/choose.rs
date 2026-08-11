use std::io::Read;

use anyhow::Context;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::cli::ChooseArgs;
use crate::color::ColorProfile;
use crate::core::duration::parse_interval;
use crate::exit::{AppError, AppResult};
use crate::theme::Palette;
use crate::ui::choose::ChooseState;
use crate::ui::echo::EchoSnapshot;
use crate::ui::key::Key;
use crate::ui::loop_::{Outcome, UiApp, UiMode, run_ui_mode};

struct ChooseApp {
    state: ChooseState,
    header: String,
    cursor: String,
    selected_prefix: String,
    unselected_prefix: String,
    multi: bool,
    /// `--ordered`, as given. The rule that a single choice also reads
    /// list order lives in `results()`, not here, so the field's name
    /// stays true to the flag.
    ordered: bool,
    show_help: bool,
    palette: Palette,
}

/// What one keystroke did, in the fewest words that still say it.
///
/// The mark under the cursor is asked about first: no key in this
/// picker both moves and toggles, but if one is ever added, a change to
/// the answer matters more than a change to where the reader is
/// looking. Only the cursor's own mark is consulted — a single-select
/// toggle clears the previous choice as part of choosing, and saying so
/// would need a second row for one keystroke.
///
/// There is deliberately no "did anything change" guard here: the
/// driver compares snapshots and does not call this at all when they
/// match. Two places deciding silence is one place too many.
fn choose_transition(
    before: &EchoSnapshot,
    now: &EchoSnapshot,
    items: &[String],
) -> Option<String> {
    let item = items.get(now.cursor)?;
    if before.marked.get(now.cursor) != now.marked.get(now.cursor) {
        return Some(if now.marked.get(now.cursor) == Some(&true) {
            format!("selected {item}")
        } else {
            format!("deselected {item}")
        });
    }
    if before.cursor != now.cursor {
        return Some(item.clone());
    }
    None
}

impl ChooseApp {
    /// What the reader can press, spelled. The painted footer says the
    /// same thing with arrow glyphs and a middot; a screen reader
    /// announces the first as nothing and the second as a word, so the
    /// transcript spells the keys and separates them with the one
    /// punctuation the vocabulary allows.
    /// The chosen items, in the order they will be printed. **The one
    /// accessor**: stdout's line and the transcript's closing row are
    /// the same list, computed once, so they cannot drift apart. Nothing
    /// else may re-derive it — a second expression of the rule below is
    /// the drift this method exists to prevent.
    ///
    /// A single choice reads list order because one choice has no
    /// selection order worth preserving.
    fn results(&self) -> Vec<String> {
        self.state.results(self.ordered || !self.multi)
    }

    fn keys_row(&self) -> String {
        let verbs = if self.multi {
            "up and down move, space selects, enter confirms, escape cancels"
        } else {
            "up and down move, enter chooses, escape cancels"
        };
        format!("{verbs}{}", crate::ui::echo::on_demand_clause(self.multi))
    }
}

impl UiApp for ChooseApp {
    fn on_key(&mut self, key: Key) -> Outcome {
        self.state.on_key(key)
    }

    fn render(&self, _area: Rect, buf: &mut Buffer) {
        let selection = Style::default().fg(self.palette.selection);
        buf.set_string(
            0,
            0,
            &self.header,
            Style::default().add_modifier(Modifier::BOLD),
        );
        let visible = usize::from(self.state.height);
        let mut y = 1;
        for idx in self.state.offset..(self.state.offset + visible).min(self.state.items.len()) {
            let at_cursor = idx == self.state.cursor;
            let mut line = String::new();
            line.push_str(if at_cursor {
                &self.cursor
            } else {
                // Keep columns aligned under the cursor marker.
                "  "
            });
            if self.multi {
                line.push_str(if self.state.selected[idx] {
                    &self.selected_prefix
                } else {
                    &self.unselected_prefix
                });
            }
            line.push_str(&self.state.items[idx]);
            let style = if at_cursor {
                selection
            } else {
                Style::default()
            };
            buf.set_string(0, y, line, style);
            y += 1;
        }
        if self.show_help {
            let help = if self.multi {
                "↑/↓ move · space select · enter confirm · esc cancel"
            } else {
                "↑/↓ move · enter choose · esc cancel"
            };
            buf.set_string(0, y, help, Style::default().add_modifier(Modifier::DIM));
        }
    }

    fn height(&self, _term: (u16, u16)) -> u16 {
        let rows = (self.state.items.len() as u16).min(self.state.height);
        1 + rows + u16::from(self.show_help)
    }

    fn echo_snapshot(&self) -> EchoSnapshot {
        EchoSnapshot {
            cursor: self.state.cursor,
            // The reducer's word is `selected`; the snapshot's is
            // `marked`, because three surfaces share this shape and
            // filter's marks are not a selection.
            marked: self.state.selected.clone(),
            ..Default::default()
        }
    }

    fn speak(&self, before: &EchoSnapshot) -> Option<String> {
        choose_transition(before, &self.echo_snapshot(), &self.state.items)
    }

    fn speak_orientation(&self) -> Vec<String> {
        let Some(item) = self.state.items.get(self.state.cursor) else {
            return vec!["no options".to_string()];
        };
        vec![format!(
            "{item} {} of {}",
            self.state.cursor + 1,
            self.state.items.len()
        )]
    }

    fn speak_selection(&self) -> Vec<String> {
        let marked = self.state.selected.iter().filter(|m| **m).count();
        if marked == 0 {
            // The zero member of one grammar, not a separate sentence.
            return vec!["nothing selected".to_string()];
        }
        // The same accessor the closing row reads, so the tagged set is
        // named in the order it will be printed. It falls back to the
        // cursor item only when nothing is marked, which the guard above
        // has already answered.
        vec![format!("{marked} selected, {}", self.results().join(", "))]
    }

    fn speak_closing(&self, outcome: &AppResult) -> Vec<String> {
        match outcome {
            Ok(()) => {
                let chosen = self.results();
                vec![if chosen.is_empty() {
                    "chose nothing".to_string()
                } else {
                    format!("chose {}", chosen.join(", "))
                }]
            }
            // Two ways of declining to choose; the transcript has no
            // reason to tell them apart. Raw mode makes the interrupt an
            // ordinary keystroke, so this row still gets written.
            Err(AppError::NoSelection) | Err(AppError::Aborted) => {
                vec!["cancelled".to_string()]
            }
            // The one ending with no keystroke behind it. The detail a
            // give-up may carry changes what stderr prints, never the
            // outcome, so the row does not read it.
            Err(AppError::Timeout(_)) => vec!["timed out".to_string()],
            // Neither is a choice the reader made: a failure has said
            // why on stderr already, and the child code belongs to
            // another command. No catch-all arm — the next variant
            // added should have to answer this question.
            Err(AppError::Fail(_)) | Err(AppError::Child(_)) => Vec::new(),
        }
    }

    fn speak_opening(&self) -> Vec<String> {
        // There is no first of nothing. An empty slot is dropped by the
        // builder, so this is the whole of the empty-list rule here.
        let position = if self.state.items.is_empty() {
            String::new()
        } else {
            format!("{} of {}", self.state.cursor + 1, self.state.items.len())
        };
        // The cap, the `first N of M` row, dropping an absent header,
        // and flattening every fragment are all the shared builder's:
        // one function builds every opening block on all three
        // surfaces, so a list of eight hundred branches costs the same
        // rows here as it does anywhere else.
        crate::ui::echo::opening_block(&self.header, &self.state.items, &position, &self.keys_row())
    }
}

pub fn run(args: ChooseArgs, profile: ColorProfile, palette: Palette, mode: UiMode) -> AppResult {
    let options = if args.options.is_empty() {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("reading stdin")?;
        let delim = if args.input_delimiter == "\\n" {
            "\n"
        } else {
            &args.input_delimiter
        };
        buf.trim_end_matches('\n')
            .split(delim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    } else {
        args.options.clone()
    };

    let out_delim = if args.output_delimiter == "\\n" {
        "\n"
    } else {
        &args.output_delimiter
    };

    if args.select_if_one && options.len() == 1 {
        println!("{}", options[0]);
        return Ok(());
    }

    let limit = if args.no_limit {
        None
    } else {
        Some(args.limit)
    };
    let multi = limit != Some(1);
    let mut state = ChooseState::new(options, limit, args.height);
    state.preselect(&args.selected);
    let mut app = ChooseApp {
        state,
        header: args.header.clone(),
        cursor: args.cursor.clone(),
        selected_prefix: args.selected_prefix.clone(),
        unselected_prefix: args.unselected_prefix.clone(),
        multi,
        ordered: args.ordered,
        show_help: !args.no_show_help,
        palette,
    };
    let timeout = args.timeout.as_deref().map(parse_interval).transpose()?;
    run_ui_mode(&mut app, profile, timeout, mode)?;

    let results = app.results();
    if !results.is_empty() {
        println!("{}", results.join(out_delim));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;
    use crate::theme::{Appearance, AppearanceSource};

    fn snap(cursor: usize, marked: &[bool]) -> EchoSnapshot {
        EchoSnapshot {
            cursor,
            marked: marked.to_vec(),
            ..Default::default()
        }
    }

    fn items4() -> Vec<String> {
        ["alpha", "beta", "gamma", "delta"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn a_move_names_the_item_and_a_toggle_names_the_mark_on_it() {
        const F: bool = false;
        const T: bool = true;
        let items = items4();
        for (n, (before, now, want)) in [
            (snap(0, &[F, F, F, F]), snap(1, &[F, F, F, F]), Some("beta")),
            (
                snap(1, &[F, F, F, F]),
                snap(0, &[F, F, F, F]),
                Some("alpha"),
            ),
            (
                snap(3, &[F, F, F, F]),
                snap(0, &[F, F, F, F]),
                Some("alpha"),
            ),
            (
                snap(1, &[F, F, F, F]),
                snap(1, &[F, T, F, F]),
                Some("selected beta"),
            ),
            (
                snap(1, &[F, T, F, F]),
                snap(1, &[F, F, F, F]),
                Some("deselected beta"),
            ),
            // A single-select toggle clears the previous choice as part
            // of choosing; only the item the reader acted on is named.
            (
                snap(1, &[T, F, F, F]),
                snap(1, &[F, T, F, F]),
                Some("selected beta"),
            ),
            (snap(1, &[F, F, F, F]), snap(1, &[F, F, F, F]), None),
            // Both changed: the mark is the louder event.
            (
                snap(0, &[F, F, F, F]),
                snap(1, &[F, T, F, F]),
                Some("selected beta"),
            ),
            // A cursor past the list declines rather than panicking.
            (snap(9, &[F, F, F, F]), snap(9, &[F, F, F, F]), None),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                choose_transition(&before, &now, &items).as_deref(),
                want,
                "row {n}"
            );
        }

        // An empty list cannot name an item.
        let empty = snap(0, &[]);
        assert_eq!(choose_transition(&empty, &empty, &[]), None);
    }

    #[test]
    fn the_snapshot_carries_the_cursor_and_the_marks_and_no_query() {
        let mut a = app(&["alpha", "beta"], true, "Choose:");
        a.state.cursor = 1;
        a.state.on_key(Key::Space);
        let snap = a.echo_snapshot();
        assert_eq!(snap.cursor, 1);
        assert_eq!(snap.marked, vec![false, true]);
        assert!(
            snap.query.is_empty(),
            "choose has no query; the field stays empty"
        );
    }

    fn app_with(state: ChooseState, multi: bool, ordered: bool, header: &str) -> ChooseApp {
        ChooseApp {
            state,
            header: header.into(),
            cursor: "> ".into(),
            selected_prefix: "[x] ".into(),
            unselected_prefix: "[ ] ".into(),
            multi,
            ordered,
            show_help: false,
            palette: Palette::builtin(Appearance::Dark, AppearanceSource::Default),
        }
    }

    fn app(items: &[&str], multi: bool, header: &str) -> ChooseApp {
        let limit = if multi { None } else { Some(1) };
        let state = ChooseState::new(items.iter().map(|s| s.to_string()).collect(), limit, 5);
        app_with(state, multi, false, header)
    }

    #[test]
    fn the_orientation_row_names_the_item_and_where_it_sits() {
        let mut a = app(&["alpha", "beta", "gamma", "delta"], false, "Choose:");
        a.state.cursor = 1;
        assert_eq!(a.speak_orientation(), ["beta 2 of 4"]);
        a.state.cursor = 0;
        assert_eq!(a.speak_orientation(), ["alpha 1 of 4"]);
        assert_eq!(
            app(&[], false, "Choose:").speak_orientation(),
            ["no options"]
        );
    }

    #[test]
    fn the_selection_row_leads_with_the_count() {
        // Nothing toggled, cursor on beta. Enter WOULD take beta, and the
        // tagged set is still empty — two questions, two answers, and
        // asserting them together is what stops the next reader from
        // "fixing" one of them.
        let mut a = app(&["alpha", "beta", "gamma", "delta"], false, "Choose:");
        a.state.cursor = 1;
        assert_eq!(a.speak_selection(), ["nothing selected"]);
        assert_eq!(a.speak_orientation(), ["beta 2 of 4"]);

        let mut one = app(&["alpha", "beta", "gamma", "delta"], true, "Choose:");
        one.state.cursor = 1;
        one.state.on_key(Key::Space);
        assert_eq!(one.speak_selection(), ["1 selected, beta"]);
        one.state.cursor = 3;
        one.state.on_key(Key::Space);
        assert_eq!(one.speak_selection(), ["2 selected, beta, delta"]);
    }

    #[test]
    fn the_closing_row_names_what_was_chosen_in_the_order_it_prints() {
        let mut a = app(&["alpha", "beta", "gamma", "delta"], true, "Choose:");
        a.state.cursor = 1;
        a.state.on_key(Key::Space);
        a.state.cursor = 0;
        a.state.on_key(Key::Space);
        assert_eq!(a.speak_closing(&Ok(())), ["chose beta, alpha"]);
        assert_eq!(a.results(), ["beta", "alpha"]);

        a.ordered = true;
        assert_eq!(a.speak_closing(&Ok(())), ["chose alpha, beta"]);
        assert_eq!(a.results(), ["alpha", "beta"]);

        let mut one = app(&["alpha", "beta"], false, "Choose:");
        one.state.cursor = 1;
        one.state.on_key(Key::Space);
        assert_eq!(one.speak_closing(&Ok(())), ["chose beta"]);

        let empty = app(&["alpha", "beta"], true, "Choose:");
        assert_eq!(empty.speak_closing(&Ok(())), ["chose nothing"]);
    }

    #[test]
    fn every_way_the_session_can_end_answers_for_itself() {
        let a = app(&["alpha", "beta"], true, "Choose:");
        assert_eq!(a.speak_closing(&Err(AppError::NoSelection)), ["cancelled"]);
        // Raw mode clears ISIG, so this arrives as a keystroke and not a
        // signal: the loop breaks normally and the row is written on the
        // way out.
        assert_eq!(a.speak_closing(&Err(AppError::Aborted)), ["cancelled"]);
        // The one ending with no keystroke behind it. Without this row
        // the session simply stops.
        assert_eq!(
            a.speak_closing(&Err(AppError::Timeout(None))),
            ["timed out"]
        );
        assert_eq!(
            a.speak_closing(&Err(AppError::Timeout(Some(anyhow::anyhow!("waiting"))))),
            ["timed out"]
        );
        // Neither is a choice the reader made; stderr has already said
        // why, and the child code belongs to another command.
        assert!(
            a.speak_closing(&Err(anyhow::anyhow!("boom").into()))
                .is_empty()
        );
        assert!(a.speak_closing(&Err(AppError::Child(7))).is_empty());
    }

    #[test]
    fn a_single_choice_reads_list_order_whatever_the_selection_order_was() {
        // DELIBERATELY unreachable through `run`, which derives
        // `multi = limit != Some(1)`. It is the only construction in
        // which the single-select half of the rule is observable: a
        // state that keeps a real selection order (no limit) under an
        // app that claims single-select. Under every reachable
        // single-select state the two orders are equal — a toggle under
        // a limit of one leaves at most one entry, and preselection
        // builds in list order — so without this fixture that half of
        // the rule could be deleted with the suite still green.
        let state = ChooseState::new(
            ["alpha", "beta", "gamma", "delta"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            None,
            5,
        );
        let mut a = app_with(state, false, false, "Choose:");
        a.state.cursor = 2;
        a.state.on_key(Key::Space);
        a.state.cursor = 0;
        a.state.on_key(Key::Space);
        assert_eq!(a.results(), ["alpha", "gamma"]);
    }

    #[test]
    fn the_opening_rows_name_the_header_the_items_the_position_and_the_keys() {
        assert_eq!(
            app(&["alpha", "beta", "gamma", "delta"], false, "Choose:").speak_opening(),
            [
                "Choose:",
                "alpha",
                "beta",
                "gamma",
                "delta",
                "1 of 4",
                "up and down move, enter chooses, escape cancels, control o says where you are",
            ]
        );

        assert_eq!(
            app(&["alpha", "beta", "gamma", "delta"], true, "Choose:").speak_opening(),
            [
                "Choose:",
                "alpha",
                "beta",
                "gamma",
                "delta",
                "1 of 4",
                "up and down move, space selects, enter confirms, escape cancels, control o says where you are, control t says what you selected",
            ]
        );
    }

    #[test]
    fn an_absent_header_opens_on_the_first_item() {
        let rows = app(&["alpha", "beta"], false, "").speak_opening();
        assert_eq!(rows[0], "alpha");
    }

    #[test]
    fn the_position_row_counts_from_where_the_cursor_actually_is() {
        // The row a literal `1 of 4` would pass: nothing opens with the
        // cursor elsewhere today, and the expression is what keeps that
        // true the day something does.
        let mut a = app(&["alpha", "beta", "gamma", "delta"], false, "Choose:");
        a.state.cursor = 2;
        assert_eq!(a.speak_opening()[5], "3 of 4");
    }

    #[test]
    fn an_empty_list_has_no_first_of_nothing() {
        assert_eq!(
            app(&[], false, "Choose:").speak_opening(),
            [
                "Choose:",
                "up and down move, enter chooses, escape cancels, control o says where you are",
            ]
        );
    }

    #[test]
    fn the_help_footer_flag_changes_nothing_about_the_opening_rows() {
        // Both of the flag's readers sit inside the painted frame, so it
        // is render-only by construction — and the keys row is read by
        // whoever is at the terminal rather than by the script author who
        // turned the footer off.
        let mut with_help = app(&["alpha", "beta"], false, "Choose:");
        with_help.show_help = true;
        let without_help = app(&["alpha", "beta"], false, "Choose:");
        assert!(!without_help.show_help);
        assert_eq!(with_help.speak_opening(), without_help.speak_opening());
    }

    fn cursor_row_fg(palette: Palette) -> Color {
        let mut app = ChooseApp {
            state: ChooseState::new(vec!["one".into(), "two".into()], Some(1), 5),
            header: "pick".into(),
            cursor: "> ".into(),
            selected_prefix: "[x] ".into(),
            unselected_prefix: "[ ] ".into(),
            multi: false,
            ordered: false,
            show_help: false,
            palette,
        };
        let area = Rect::new(0, 0, 40, 6);
        let mut buf = Buffer::empty(area);
        app.state.cursor = 0;
        app.render(area, &mut buf);
        buf.cell((0, 1)).expect("first row is painted").fg
    }

    fn rendered(palette: Palette) -> Vec<String> {
        let mut app = ChooseApp {
            state: ChooseState::new(vec!["one".into(), "two".into()], Some(1), 5),
            header: "pick".into(),
            cursor: "> ".into(),
            selected_prefix: "[x] ".into(),
            unselected_prefix: "[ ] ".into(),
            multi: false,
            ordered: false,
            show_help: false,
            palette,
        };
        let area = Rect::new(0, 0, 40, 6);
        let mut buf = Buffer::empty(area);
        app.state.cursor = 0;
        app.render(area, &mut buf);
        crate::term::buffer_ansi::buffer_to_lines(&buf, ColorProfile::Ansi256)
    }

    #[test]
    fn the_rendered_choose_frame_is_pinned() {
        // Byte-identity golden: captured on v0.5.0 render code, before the
        // selection-token rewire. If this changes, emitted bytes changed.
        let dark = rendered(Palette::builtin(
            Appearance::Dark,
            AppearanceSource::Default,
        ));
        assert_eq!(
            dark,
            [
                "\u{1b}[1mpick\u{1b}[0m",
                "\u{1b}[38;5;212m> one\u{1b}[0m",
                "  two",
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
                "\u{1b}[1mpick\u{1b}[0m",
                "\u{1b}[38;5;129m> one\u{1b}[0m",
                "  two",
                "",
                "",
                ""
            ]
        );
    }

    #[test]
    fn the_cursor_row_reads_the_selection_token_not_the_accent() {
        // Sentinel: selection and accent share a value by construction, so
        // only a diverging palette proves the render reads `selection`.
        let palette = Palette {
            selection: Color::Indexed(99),
            ..Palette::builtin(Appearance::Dark, AppearanceSource::Default)
        };
        assert_eq!(cursor_row_fg(palette), Color::Indexed(99));
    }

    #[test]
    fn the_cursor_row_takes_its_selection_from_the_palette() {
        let dark = Palette::builtin(Appearance::Dark, AppearanceSource::Default);
        let light = Palette::builtin(Appearance::Light, AppearanceSource::Default);
        assert_eq!(cursor_row_fg(dark), dark.selection);
        // The literal is the byte-identity pin; the field name is routing.
        assert_eq!(cursor_row_fg(dark), Color::Indexed(212));
        assert_eq!(cursor_row_fg(light), light.selection);
        assert_ne!(cursor_row_fg(dark), cursor_row_fg(light));
    }
}
