use std::io::Read;

use anyhow::Context;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::cli::ChooseArgs;
use crate::color::ColorProfile;
use crate::core::duration::parse_interval;
use crate::exit::AppResult;
use crate::theme::Palette;
use crate::ui::choose::ChooseState;
use crate::ui::key::Key;
use crate::ui::loop_::{Outcome, UiApp, UiMode, run_ui_mode};

struct ChooseApp {
    state: ChooseState,
    header: String,
    cursor: String,
    selected_prefix: String,
    unselected_prefix: String,
    multi: bool,
    show_help: bool,
    palette: Palette,
}

impl ChooseApp {
    /// What the reader can press, spelled. The painted footer says the
    /// same thing with arrow glyphs and a middot; a screen reader
    /// announces the first as nothing and the second as a word, so the
    /// transcript spells the keys and separates them with the one
    /// punctuation the vocabulary allows.
    fn keys_row(&self) -> String {
        if self.multi {
            "up and down move, space selects, enter confirms, escape cancels".to_string()
        } else {
            "up and down move, enter chooses, escape cancels".to_string()
        }
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
        show_help: !args.no_show_help,
        palette,
    };
    let timeout = args.timeout.as_deref().map(parse_interval).transpose()?;
    run_ui_mode(&mut app, profile, timeout, mode)?;

    let results = app.state.results(args.ordered || !multi);
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

    fn app(items: &[&str], multi: bool, header: &str) -> ChooseApp {
        ChooseApp {
            state: ChooseState::new(
                items.iter().map(|s| s.to_string()).collect(),
                if multi { None } else { Some(1) },
                5,
            ),
            header: header.into(),
            cursor: "> ".into(),
            selected_prefix: "[x] ".into(),
            unselected_prefix: "[ ] ".into(),
            multi,
            show_help: false,
            palette: Palette::builtin(Appearance::Dark, AppearanceSource::Default),
        }
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
                "up and down move, enter chooses, escape cancels",
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
                "up and down move, space selects, enter confirms, escape cancels",
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
            ["Choose:", "up and down move, enter chooses, escape cancels",]
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
