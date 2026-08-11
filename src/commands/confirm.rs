use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use unicode_width::UnicodeWidthStr;

use crate::cli::ConfirmArgs;
use crate::color::ColorProfile;
use crate::core::duration::parse_interval;
use crate::exit::{AppError, AppResult};
use crate::theme::Palette;
use crate::ui::confirm::ConfirmState;
use crate::ui::echo::EchoSnapshot;
use crate::ui::key::Key;
use crate::ui::loop_::{Outcome, UiApp, UiMode, run_ui_mode};

struct ConfirmApp {
    state: ConfirmState,
    prompt: String,
    affirmative: String,
    negative: String,
    palette: Palette,
}

impl ConfirmApp {
    /// The armed answer's label — the one string this command names,
    /// read by the printed output and by the spoken closing row alike.
    /// Two derivations of "which one is armed" would agree until the day
    /// one of them was corrected.
    fn label(&self) -> &str {
        if self.state.affirmative {
            &self.affirmative
        } else {
            &self.negative
        }
    }

    /// What the reader can press, spelled. The painted buttons say the
    /// same thing with position and color, neither of which a screen
    /// reader announces, so the transcript spells the keys and separates
    /// them with the one punctuation the vocabulary allows.
    fn keys_row(&self) -> String {
        format!(
            "left and right move, y and n answer, enter chooses, escape cancels{}",
            // There is no marked set to have here.
            crate::ui::echo::on_demand_clause(false)
        )
    }
}

impl UiApp for ConfirmApp {
    fn on_key(&mut self, key: Key) -> Outcome {
        self.state.on_key(key)
    }

    fn render(&self, _area: Rect, buf: &mut Buffer) {
        buf.set_string(
            0,
            0,
            &self.prompt,
            Style::default().add_modifier(Modifier::BOLD),
        );
        let active = Style::default()
            .fg(self.palette.on_accent)
            .bg(self.palette.accent)
            .add_modifier(Modifier::BOLD);
        let inactive = Style::default().add_modifier(Modifier::DIM);
        let yes = format!(" {} ", self.affirmative);
        let no = format!(" {} ", self.negative);
        let (yes_style, no_style) = if self.state.affirmative {
            (active, inactive)
        } else {
            (inactive, active)
        };
        buf.set_string(2, 1, &yes, yes_style);
        // Cells, not bytes: a non-ASCII label spans fewer columns than it
        // does bytes, and the buffer is addressed in columns.
        let no_x = 2 + yes.as_str().width() as u16 + 3;
        buf.set_string(no_x, 1, &no, no_style);
    }

    fn height(&self, _term: (u16, u16)) -> u16 {
        2
    }

    fn echo_snapshot(&self) -> EchoSnapshot {
        // Yes sits on the left, so it is cursor 0. A two-button control
        // has no marks and nothing to type into.
        EchoSnapshot {
            cursor: usize::from(!self.state.affirmative),
            ..EchoSnapshot::default()
        }
    }

    fn speak_opening(&self) -> Vec<String> {
        crate::ui::echo::opening_block(
            &self.prompt,
            &[self.affirmative.clone(), self.negative.clone()],
            // Where you are, in the words a later toggle will use: the
            // armed answer. A coordinate would name a position in a list
            // of two unnamed things.
            self.label(),
            &self.keys_row(),
        )
    }

    fn speak_orientation(&self) -> Vec<String> {
        vec![format!(
            "{} {} of 2",
            self.label(),
            usize::from(!self.state.affirmative) + 1
        )]
    }

    fn speak_selection(&self) -> Vec<String> {
        // There is no marked set here and an answer is always active, so
        // the zero member of the grammar would be false. One grammar
        // across the surfaces, and a bare label would be byte-identical
        // to this surface's own toggle row.
        vec![format!("1 selected, {}", self.label())]
    }

    fn speak_closing(&self, outcome: &AppResult) -> Vec<String> {
        match outcome {
            // A negative answer submits like an affirmative one; only
            // the exit code differs.
            Ok(()) => vec![format!("chose {}", self.label())],
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

    fn speak(&self, _before: &EchoSnapshot) -> Option<String> {
        // The answer that is armed NOW. What was armed before is what
        // decided there was anything to say; the row is the resting
        // state, which is what keeps a coalesced row true.
        Some(self.label().to_string())
    }
}

pub fn run(args: ConfirmArgs, profile: ColorProfile, palette: Palette, mode: UiMode) -> AppResult {
    let mut app = ConfirmApp {
        state: ConfirmState {
            affirmative: args.default_yes,
        },
        prompt: args.prompt.clone(),
        affirmative: args.affirmative.clone(),
        negative: args.negative.clone(),
        palette,
    };
    let timeout = args.timeout.as_deref().map(parse_interval).transpose()?;
    run_ui_mode(&mut app, profile, timeout, mode)?;

    if args.show_output {
        println!("{}", app.label());
    }
    if app.state.affirmative {
        Ok(())
    } else {
        Err(AppError::NoSelection)
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;
    use crate::theme::{Appearance, AppearanceSource};

    fn app(affirmative: bool) -> ConfirmApp {
        ConfirmApp {
            state: ConfirmState { affirmative },
            prompt: "Ship it?".into(),
            affirmative: "Yes".into(),
            negative: "No".into(),
            palette: Palette::builtin(Appearance::Dark, AppearanceSource::Default),
        }
    }

    #[test]
    fn the_on_demand_rows_name_the_armed_answer() {
        assert_eq!(app(true).speak_orientation(), ["Yes 1 of 2"]);
        assert_eq!(app(false).speak_orientation(), ["No 2 of 2"]);
        // There is no marked set here, and an answer is always active —
        // so "nothing selected" would be false.
        assert_eq!(app(true).speak_selection(), ["1 selected, Yes"]);
        assert_eq!(app(false).speak_selection(), ["1 selected, No"]);
    }

    #[test]
    fn every_way_the_session_can_end_answers_for_itself() {
        // A negative answer is a submit too — only the exit code
        // differs — so it names its label rather than reading as a
        // cancel.
        assert_eq!(app(true).speak_closing(&Ok(())), ["chose Yes"]);
        assert_eq!(app(false).speak_closing(&Ok(())), ["chose No"]);

        let a = app(true);
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

    fn active_button(palette: Palette) -> (Color, Color) {
        let app = ConfirmApp {
            state: ConfirmState { affirmative: true },
            prompt: "Ship it?".into(),
            affirmative: "Yes".into(),
            negative: "No".into(),
            palette,
        };
        let area = Rect::new(0, 0, 40, 2);
        let mut buf = Buffer::empty(area);
        app.render(area, &mut buf);
        let cell = buf.cell((2, 1)).expect("the active button is painted");
        (cell.fg, cell.bg)
    }

    #[test]
    fn the_second_button_is_placed_by_cells_not_bytes() {
        // `yes.len()` is a BYTE count: a non-ASCII affirmative pushed the
        // negative button right by the difference between its bytes and
        // its display width.
        let app = ConfirmApp {
            state: ConfirmState { affirmative: true },
            prompt: "Ship it?".into(),
            affirmative: "はい".into(),
            negative: "No".into(),
            palette: Palette::builtin(Appearance::Dark, AppearanceSource::Default),
        };
        let area = Rect::new(0, 0, 40, 2);
        let mut buf = Buffer::empty(area);
        app.render(area, &mut buf);
        // " はい " spans 6 cells from column 2, so the gap runs 8..11 and
        // the negative button opens at 11.
        assert_eq!(buf.cell((8, 1)).unwrap().symbol(), " ", "gap after yes");
        assert_eq!(buf.cell((11, 1)).unwrap().symbol(), " ", "no button pad");
        assert_eq!(buf.cell((12, 1)).unwrap().symbol(), "N");
    }

    fn rendered(palette: Palette) -> Vec<String> {
        let app = ConfirmApp {
            state: ConfirmState { affirmative: true },
            prompt: "Ship it?".into(),
            affirmative: "Yes".into(),
            negative: "No".into(),
            palette,
        };
        let area = Rect::new(0, 0, 40, 2);
        let mut buf = Buffer::empty(area);
        app.render(area, &mut buf);
        crate::term::buffer_ansi::buffer_to_lines(&buf, ColorProfile::Ansi256)
    }

    #[test]
    fn the_rendered_confirm_frame_is_pinned() {
        // The active button's foreground is the cube-pinned on-accent
        // (16 / 231), deliberately not a named ANSI color: terminal themes
        // remap the ANSI-16 palette (a light theme's "bright white" is
        // usually gray), which broke the pairing over the absolute accent.
        let dark = rendered(Palette::builtin(
            Appearance::Dark,
            AppearanceSource::Default,
        ));
        assert_eq!(
            dark,
            [
                "\u{1b}[1mShip it?\u{1b}[0m",
                "  \u{1b}[1;38;5;16;48;5;212m Yes \u{1b}[0m   \u{1b}[2m No \u{1b}[0m"
            ]
        );
        let light = rendered(Palette::builtin(
            Appearance::Light,
            AppearanceSource::Default,
        ));
        assert_eq!(
            light,
            [
                "\u{1b}[1mShip it?\u{1b}[0m",
                "  \u{1b}[1;38;5;231;48;5;129m Yes \u{1b}[0m   \u{1b}[2m No \u{1b}[0m"
            ]
        );
    }

    #[test]
    fn the_active_button_stays_on_the_brand_accent() {
        // Deliberately NOT the selection token: the button is a
        // brand-accented control, not a list selection, and on-accent is
        // documented as the text drawn on `accent`. A sentinel selection
        // must leave the button untouched.
        let palette = Palette {
            selection: Color::Indexed(99),
            ..Palette::builtin(Appearance::Dark, AppearanceSource::Default)
        };
        let (_, bg) = active_button(palette);
        assert_eq!(bg, Color::Indexed(212));
    }

    #[test]
    fn the_active_button_pairs_on_accent_over_accent() {
        let dark = Palette::builtin(Appearance::Dark, AppearanceSource::Default);
        let light = Palette::builtin(Appearance::Light, AppearanceSource::Default);
        assert_eq!(
            active_button(dark),
            (Color::Indexed(16), Color::Indexed(212))
        );
        assert_eq!(active_button(light), (light.on_accent, light.accent));
    }
}
