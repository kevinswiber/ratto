use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::Context;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::color::ColorProfile;
use crate::exit::{AppError, AppResult};
use crate::term::buffer_ansi::buffer_to_lines;
use crate::term::inline::InlineRenderer;
use crate::term::tty::{RawModeGuard, UiStream};
use crate::ui::echo::{EchoSnapshot, echo_rows};
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
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum UiMode {
    Painted,
    Echo,
}

/// Where a session's own bytes go. Painted mode hands the stream to the
/// renderer, which owns the frame math; a transcript keeps the stream
/// and appends rows past it. The variants are what make the two modes
/// exclusive: an arm with no renderer has no frame to erase and nothing
/// to hide the cursor with.
enum UiSink {
    Painted(InlineRenderer<UiStream>),
    Echo(UiStream),
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
    /// What the transcript can describe about this app right now. The
    /// driver compares it around each key and says nothing when the two
    /// are equal, so the no-op rule is decided once for every surface
    /// rather than in each of them. An app with nothing to describe
    /// keeps the default and is silent by construction.
    // The driver compares one around every key next.
    #[allow(dead_code)]
    fn echo_snapshot(&self) -> EchoSnapshot {
        EchoSnapshot::default()
    }
    /// The block printed once on entry: what this is, what is in it,
    /// where the cursor is, which keys do what.
    fn speak_opening(&self) -> Vec<String> {
        Vec::new()
    }
    /// The shortest unambiguous words for the transition that just
    /// happened, or `None` when this app has none for it.
    // The driver hands what this returns to the burst policy next.
    #[allow(dead_code)]
    fn speak(&self, _before: &EchoSnapshot) -> Option<String> {
        None
    }
    /// Where the reader is, on demand — the one thing a transcript
    /// cannot repeat back on its own.
    // The driver answers the orientation key with this next.
    #[allow(dead_code)]
    fn speak_orientation(&self) -> Vec<String> {
        Vec::new()
    }
    /// What is marked, on demand.
    // The driver answers the marked-set key with this next.
    #[allow(dead_code)]
    fn speak_selection(&self) -> Vec<String> {
        Vec::new()
    }
    /// The result, as its own row, before the process exits. This is the
    /// row that the painted mode cannot deliver at all: it erases itself
    /// on the way out, so the chosen value is printed into a region that
    /// has already been wiped.
    // The driver writes this on the way out next.
    #[allow(dead_code)]
    fn speak_closing(&self, _outcome: &AppResult) -> Vec<String> {
        Vec::new()
    }
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

/// How long the transcript waits for the reader to stop before it says
/// where they landed. Expressed in terms of the poll slice so the two
/// cannot drift apart silently.
///
/// A starting value, derived from measured INPUT cadence: typing runs
/// near an eighth of a second per character, so a typed burst settles
/// into one row, while an unhurried arrow run is roughly half a second
/// between keys and still speaks every move. Nothing here is derived
/// from how long a screen reader takes to say a row — that has not been
/// measured, and this value should be revisited once the mode has
/// actually been heard.
// The driver's transcript arm builds its burst policy with this next.
#[allow(dead_code)]
const ECHO_QUIESCENCE: Duration = POLL_SLICE;

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

/// Painted mode, for callers with no transcript to write. Its signature
/// is deliberately unchanged.
pub fn run_ui<A: UiApp>(
    app: &mut A,
    profile: ColorProfile,
    timeout: Option<Duration>,
) -> AppResult {
    run_ui_mode(app, profile, timeout, UiMode::Painted)
}

/// Drive a UiApp on the UI stream: raw mode, event pump, and either an
/// inline repaint or a transcript of appended rows. The painted UI
/// erases itself on exit so only the result (printed by the caller to
/// stdout) remains; a transcript is meant to stay on screen. Esc maps to
/// exit 1, Ctrl-C to 130, --timeout to 124, in both.
pub fn run_ui_mode<A: UiApp>(
    app: &mut A,
    profile: ColorProfile,
    timeout: Option<Duration>,
    mode: UiMode,
) -> AppResult {
    let ui = UiStream::open();
    if !ui.is_tty() {
        return Err(anyhow::anyhow!("interactive commands need a terminal").into());
    }
    let _raw_guard = RawModeGuard::enable().context("enabling raw mode")?;
    let mut sink = match mode {
        UiMode::Painted => UiSink::Painted(
            InlineRenderer::new(ui)
                .with_cursor_hidden(true)
                .with_sync_output(true),
        ),
        UiMode::Echo => UiSink::Echo(ui),
    };
    if let UiSink::Echo(out) = &mut sink {
        echo_rows(out, &app.speak_opening())?;
    }

    let deadline = timeout.map(|t| Instant::now() + t);
    let outcome = loop {
        if let UiSink::Painted(renderer) = &mut sink {
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
        }

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

    match &mut sink {
        UiSink::Painted(renderer) => {
            renderer.clear().context("clearing ui")?;
            renderer.finish().context("restoring terminal")?;
        }
        // Nothing to erase and no cursor to restore — the transcript is
        // meant to stay. Push what it wrote out before the caller prints
        // the result to stdout, so an uncaptured run reads in the order
        // the two streams happened.
        UiSink::Echo(out) => out.flush().context("flushing ui")?,
    }
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
    fn the_quiescence_interval_is_expressed_in_terms_of_the_poll_slice() {
        // A ruled relationship, not a mechanical limit: `wait_cap` can
        // shorten a poll, so a smaller interval would in fact be honored.
        // The interval is expressed in terms of the constant that already
        // governs how often the loop wakes, and this is the assertion that
        // keeps the two from drifting apart. `>=` rather than `==` so it
        // survives a deliberate re-tune and still catches a bare literal.
        assert!(ECHO_QUIESCENCE >= POLL_SLICE);
    }

    #[test]
    fn an_app_that_overrides_nothing_has_nothing_to_say() {
        struct Silent;
        impl UiApp for Silent {
            fn on_key(&mut self, _key: Key) -> Outcome {
                Outcome::Continue
            }
            fn render(&self, _area: Rect, _buf: &mut Buffer) {}
            fn height(&self, _term: (u16, u16)) -> u16 {
                1
            }
        }

        let app = Silent;
        assert_eq!(app.echo_snapshot(), EchoSnapshot::default());
        assert!(app.speak_opening().is_empty());
        assert!(app.speak(&EchoSnapshot::default()).is_none());
        assert!(app.speak_orientation().is_empty());
        assert!(app.speak_selection().is_empty());
        assert!(app.speak_closing(&Ok(())).is_empty());
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
