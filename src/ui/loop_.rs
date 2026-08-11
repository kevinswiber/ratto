use std::time::{Duration, Instant};

use anyhow::Context;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::color::ColorProfile;
use crate::exit::{AppError, AppResult};
use crate::term::buffer_ansi::buffer_to_lines;
use crate::term::inline::InlineRenderer;
use crate::term::tty::{RawModeGuard, UiStream};
use crate::ui::key::{Key, from_crossterm};

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    Continue,
    Submit,
    Abort,
}

/// How a session presents itself: the painted inline frame, or a
/// transcript of spoken rows appended to the UI stream. Resolved once
/// per process and threaded by value, so nothing downstream has to ask
/// the environment a second time and get a different answer.
// Built by `real_main`, which resolves the presentation once and hands
// it to the command it runs. The attribute is temporary and goes with
// the commit that adds that call.
#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum UiMode {
    Painted,
    Echo,
}

/// An interactive command: a pure reducer plus a widget render.
pub trait UiApp {
    fn on_key(&mut self, key: Key) -> Outcome;
    fn render(&self, area: Rect, buf: &mut Buffer);
    fn height(&self, term: (u16, u16)) -> u16;
    /// The frame cell (column, row) where the hardware cursor should
    /// rest after painting. Screen readers and braille displays track
    /// only the real cursor, so any app with an edit point must return
    /// it here; `None` keeps the cursor hidden (right for list UIs).
    fn cursor_pos(&self) -> Option<(u16, u16)> {
        None
    }
    /// Learn the terminal size before painting. An app with a scrolling
    /// field needs the width to place its window, and `render` cannot
    /// mutate. Called every iteration, so a resize lands here too.
    fn prepare(&mut self, _term: (u16, u16)) {}
}

/// Where the hardware cursor may actually be parked. A scrolling field
/// keeps its caret inside the frame, so this is a guard rather than a
/// placement: a caret past the last column has no cell of its own, and
/// clamping it there would park it *on* the final character instead of
/// after it. Decline the park instead of lying about it.
fn park_target(cursor: Option<(u16, u16)>, cols: u16, height: u16) -> Option<(u16, u16)> {
    cursor.filter(|&(col, row)| col < cols && row < height)
}

/// Rank the two ways of asking for a transcript against the one way of
/// refusing one. The refusal wins outright. Otherwise the request
/// decides, and it arrives in three states, not two: nobody asked, or
/// someone asked, or someone explicitly refused — a value of `false`
/// must read as a refusal, never as "a value was present".
///
/// The flag and the ambient variable have already been ranked against
/// each other by the argument parser, which prefers what was typed on
/// the command line; this function never reads the environment itself.
// Called once, from `real_main`, as soon as the command line carries the
// two switches it ranks. The attribute goes with that call.
#[allow(dead_code)]
pub fn resolve_ui_mode(no_accessible: bool, accessible: Option<bool>) -> UiMode {
    if no_accessible {
        return UiMode::Painted;
    }
    match accessible {
        Some(true) => UiMode::Echo,
        Some(false) | None => UiMode::Painted,
    }
}

/// How long the event pump may block before it must look at the clock
/// again. One value for both of the waits below: a second literal is a
/// second fact that drifts the first time one of them is tuned.
const POLL_SLICE: Duration = Duration::from_millis(250);

/// How long to wait for the next key, or `None` when the deadline has
/// already passed and the run is over. A nearer deadline shortens the
/// slice; nothing lengthens it.
fn poll_wait(deadline: Option<Instant>, now: Instant) -> Option<Duration> {
    match deadline {
        Some(d) if now >= d => None,
        Some(d) => Some((d - now).min(POLL_SLICE)),
        None => Some(POLL_SLICE),
    }
}

/// Drive a UiApp on the UI stream: raw mode, event pump, inline repaint.
/// The UI erases itself on exit so only the result (printed by the caller
/// to stdout) remains. Esc maps to exit 1, Ctrl-C to 130, --timeout to 124.
pub fn run_ui<A: UiApp>(
    app: &mut A,
    profile: ColorProfile,
    timeout: Option<Duration>,
) -> AppResult {
    let ui = UiStream::open();
    if !ui.is_tty() {
        return Err(anyhow::anyhow!("interactive commands need a terminal").into());
    }
    let _raw_guard = RawModeGuard::enable().context("enabling raw mode")?;
    let mut renderer = InlineRenderer::new(ui)
        .with_cursor_hidden(true)
        .with_sync_output(true);

    let deadline = timeout.map(|t| Instant::now() + t);
    let outcome = loop {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        app.prepare((cols, rows));
        let height = app.height((cols, rows)).clamp(1, rows);
        let area = Rect::new(0, 0, cols, height);
        let mut buf = Buffer::empty(area);
        app.render(area, &mut buf);
        let cursor = park_target(app.cursor_pos(), cols, height);
        renderer
            .draw_with_cursor(&buffer_to_lines(&buf, profile), cols, cursor)
            .context("painting ui")?;

        let Some(wait) = poll_wait(deadline, Instant::now()) else {
            break Err(AppError::Timeout(None));
        };
        if crossterm::event::poll(wait).context("polling events")? {
            let event = crossterm::event::read().context("reading event")?;
            if let crossterm::event::Event::Key(key_event) = event {
                let Some(key) = from_crossterm(key_event) else {
                    continue;
                };
                if key == Key::CtrlC {
                    break Err(AppError::Aborted);
                }
                match app.on_key(key) {
                    Outcome::Continue => {}
                    Outcome::Submit => break Ok(()),
                    Outcome::Abort => break Err(AppError::NoSelection),
                }
            }
        }
    };

    renderer.clear().context("clearing ui")?;
    renderer.finish().context("restoring terminal")?;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_caret_inside_the_frame_parks_where_it_asked() {
        assert_eq!(park_target(Some((0, 0)), 20, 2), Some((0, 0)));
        assert_eq!(park_target(Some((19, 1)), 20, 2), Some((19, 1)));
    }

    #[test]
    fn a_caret_past_the_last_column_declines_rather_than_lying() {
        // Clamping to cols-1 would park the cursor ON the final
        // character; there is no honest cell, so no park.
        assert_eq!(park_target(Some((20, 0)), 20, 2), None);
        assert_eq!(park_target(Some((99, 0)), 20, 2), None);
    }

    #[test]
    fn a_caret_below_the_frame_declines_too() {
        assert_eq!(park_target(Some((0, 2)), 20, 2), None);
    }

    #[test]
    fn no_caret_stays_no_caret() {
        assert_eq!(park_target(None, 20, 2), None);
    }

    #[test]
    fn the_mode_answers_every_shape_of_the_two_switches() {
        // The whole product of what can reach here: the off switch,
        // against the three states of a request — nobody asked, someone
        // asked, someone refused. Six pairs, and the transcript is one
        // of them.
        for (off, asked, want) in [
            (false, None, UiMode::Painted),
            (false, Some(true), UiMode::Echo),
            (false, Some(false), UiMode::Painted),
            (true, None, UiMode::Painted),
            (true, Some(true), UiMode::Painted),
            (true, Some(false), UiMode::Painted),
        ] {
            assert_eq!(
                resolve_ui_mode(off, asked),
                want,
                "off={off} asked={asked:?}"
            );
        }
    }

    #[test]
    fn the_off_switch_outranks_a_request() {
        // A user who turned the transcript on in their shell profile and
        // then typed the off switch on this one command means the off
        // switch. A resolver that answers the request first and checks
        // the refusal after gets exactly this case wrong, and gets every
        // other case right.
        assert_eq!(resolve_ui_mode(true, Some(true)), UiMode::Painted);
    }

    #[test]
    fn a_refused_request_is_not_an_absent_one() {
        // A refusal and an absence reach the same mode by different
        // routes, and a resolver that reads presence rather than value
        // turns a deliberate opt-out into an opt-in — the failure whose
        // symptom is that the switch does the opposite of what it says.
        assert_eq!(resolve_ui_mode(false, Some(false)), UiMode::Painted);
        assert_eq!(resolve_ui_mode(false, None), UiMode::Painted);
    }

    #[test]
    fn both_wait_arms_reach_for_the_same_slice() {
        // What was two literals is one value, and this is the assertion
        // that notices if it becomes two again: an untimed run and a run
        // whose deadline is far away must wait the same amount.
        let now = Instant::now();
        let far = now + Duration::from_secs(60);
        assert_eq!(poll_wait(None, now), Some(POLL_SLICE));
        assert_eq!(poll_wait(Some(far), now), Some(POLL_SLICE));
    }

    #[test]
    fn a_nearer_deadline_shortens_the_wait_and_an_expired_one_ends_it() {
        // The timeout boundary, which has never had a test. `None` is
        // the caller's signal to stop, and the comparison is inclusive:
        // a deadline reached exactly is a deadline passed.
        let now = Instant::now();
        assert_eq!(
            poll_wait(Some(now + Duration::from_millis(30)), now),
            Some(Duration::from_millis(30)),
            "a deadline inside the slice wins"
        );
        assert_eq!(poll_wait(Some(now), now), None, "reached is passed");
        assert_eq!(poll_wait(Some(now - Duration::from_millis(1)), now), None);
    }
}
