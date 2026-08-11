#![cfg(unix)]
mod common;

use std::time::Duration;

use common::pty::{
    ECHO_FORBIDDEN, FakeTerminal, PtySession, SILENCE_WINDOW, after, assert_no_echo_escapes,
    assert_silent_then_alive, before, contains, count, drain_for, press_and_hear,
    try_wait_for_in_order, wait_for_in_order,
};

/// Path to the rat binary — mirrors `tests/pty_watch.rs`'s local
/// `rat_bin()` per that file's precedent.
fn rat_bin() -> String {
    assert_cmd::cargo::cargo_bin("rat").display().to_string()
}

/// The slice strictly between the first `open` and the first `close`
/// after it — for counting rows inside a known span.
fn between<'a>(haystack: &'a [u8], open: &[u8], close: &[u8]) -> &'a [u8] {
    let start = haystack
        .windows(open.len())
        .position(|w| w == open)
        .map(|at| at + open.len())
        .expect("the opening needle must be present");
    let end = haystack[start..]
        .windows(close.len())
        .position(|w| w == close)
        .expect("the closing needle must be present");
    &haystack[start..start + end]
}

const KEYS_ONE: &[u8] =
    b"up and down move, enter chooses, escape cancels, control o says where you are\r\n";
const KEYS_MULTI: &[u8] =
    b"up and down move, space selects, enter confirms, escape cancels, control o says where you are, control t says what you selected\r\n";

/// Run a choose session that is expected to paint, and return the bytes
/// of its first frame. Shuts the session down before returning, so a
/// failing assertion in the caller can never leave a process behind.
fn painted_choose(flags: &[&str], envs: &[(&str, &str)]) -> Vec<u8> {
    let mut argv = vec!["choose"];
    argv.extend_from_slice(flags);
    argv.extend_from_slice(&["alpha", "beta"]);
    let session = PtySession::spawn(&rat_bin(), &argv, envs).expect("spawn rat choose under a pty");
    let mut terminal = FakeTerminal::dark();
    let seen = try_wait_for_in_order(
        &session,
        &mut terminal,
        &[b"\x1b[?25l"],
        Duration::from_secs(5),
    );
    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(2));
    seen.expect("a painting session must hide the cursor on its first frame")
}

/// The control that gives every transcript escape check its meaning: the
/// same list, required to MATCH here. Membership is asserted first, so a
/// ban entry that drifts into a sequence the renderer never writes fails
/// loudly instead of retiring itself.
fn assert_frame_escapes_present(bytes: &[u8], context: &str) {
    for escape in [b"\x1b[?25l".as_slice(), b"\x1b[0J".as_slice()] {
        assert!(
            ECHO_FORBIDDEN.contains(&escape),
            "the control names {:?}, which the ban no longer lists",
            String::from_utf8_lossy(escape)
        );
        assert!(
            contains(bytes, escape),
            "{context}: expected {:?} in {:?}",
            String::from_utf8_lossy(escape),
            String::from_utf8_lossy(bytes)
        );
    }
}

#[test]
fn an_accessible_choose_paints_nothing_and_leaves_when_asked() {
    let session = PtySession::spawn(
        &rat_bin(),
        &["choose", "--accessible", "alpha", "beta"],
        // A pinned appearance suppresses the startup probe, so every
        // escape in the stream is the session's own.
        &[("RAT_APPEARANCE", "dark")],
    )
    .expect("spawn rat choose under a pty");

    // Options come from the command line, so nothing is read from stdin
    // and the terminal never echoes the test's own bytes back.
    let seen = drain_for(&session, Duration::from_millis(700));
    // Liveness first: an empty stream from a process that never started
    // would satisfy the escape check for the wrong reason.
    assert!(
        !session.exited(),
        "the session must still be waiting for a key"
    );
    assert_no_echo_escapes(&seen, "a transcript session at rest");

    session.write_bytes(b"\x1b");
    assert!(
        !session.kill_if_alive(Duration::from_secs(2)),
        "escape must end the session without a kill"
    );
}

#[test]
fn an_accessible_choose_times_out_without_painting() {
    let session = PtySession::spawn(
        &rat_bin(),
        &[
            "choose",
            "--accessible",
            "--timeout",
            "300ms",
            "alpha",
            "beta",
        ],
        &[("RAT_APPEARANCE", "dark")],
    )
    .expect("spawn rat choose under a pty");
    let mut terminal = FakeTerminal::dark();

    // The give-up notice is written to stderr, which shares this pty —
    // so it is both the proof that the loop really ran to its deadline
    // and the needle that ends the capture. `wait_for_in_order` is the
    // helper that RETURNS the accumulated stream; `wait_for` returns only
    // a bool and consumes what it read, which would leave the assertion
    // below inspecting an empty tail and passing for that reason.
    let seen = wait_for_in_order(
        &session,
        &mut terminal,
        &[b"timed out"],
        Duration::from_secs(5),
    );
    assert_no_echo_escapes(&seen, "a transcript session giving up");
    assert!(!session.kill_if_alive(Duration::from_secs(2)));
}

#[test]
fn the_painted_default_writes_what_a_transcript_may_not() {
    // Without this, the transcript's escape check is green whenever the
    // banned bytes are ones nothing ever writes.
    let seen = painted_choose(&[], &[("RAT_APPEARANCE", "dark")]);
    assert_frame_escapes_present(&seen, "the default painted path");
}

#[test]
fn the_off_switch_paints() {
    let seen = painted_choose(&["--no-accessible"], &[("RAT_APPEARANCE", "dark")]);
    assert_frame_escapes_present(&seen, "an explicitly refused transcript");
}

#[test]
fn a_refusing_variable_paints() {
    // The variable's false spellings are the parser's; this asserts the
    // one a shell user actually types.
    let seen = painted_choose(&[], &[("RAT_APPEARANCE", "dark"), ("RAT_ACCESSIBLE", "0")]);
    assert_frame_escapes_present(&seen, "a session the environment turned off");
}

#[test]
fn the_flag_outranks_a_refusing_variable() {
    // What is typed on the command line beats what the shell exported.
    // This is the one relation the argument parser supplies rather than
    // the resolver, so it is asserted against the shipped binary.
    let session = PtySession::spawn(
        &rat_bin(),
        &["choose", "--accessible", "alpha", "beta"],
        &[("RAT_APPEARANCE", "dark"), ("RAT_ACCESSIBLE", "0")],
    )
    .expect("spawn rat choose under a pty");

    let seen = drain_for(&session, Duration::from_millis(700));
    assert!(
        !session.exited(),
        "the session must still be waiting for a key"
    );
    assert_no_echo_escapes(&seen, "a flag that outranked the environment");

    session.write_bytes(b"\x1b");
    assert!(!session.kill_if_alive(Duration::from_secs(2)));
}

#[test]
fn the_off_switch_outranks_an_asking_variable() {
    // The top rung: a user who turned the transcript on in their profile
    // and typed the off switch on this one command gets a painted list.
    let seen = painted_choose(
        &["--no-accessible"],
        &[("RAT_APPEARANCE", "dark"), ("RAT_ACCESSIBLE", "1")],
    );
    assert_frame_escapes_present(&seen, "an off switch over an asking environment");
}

#[test]
fn an_empty_variable_does_not_stop_the_command() {
    // `export RAT_ACCESSIBLE=` is one line in a shell profile, and one
    // unset expansion away from being written by accident. It must mean
    // "not asking" — so this session paints, and above all it RUNS.
    // With a strict boolean parser it would exit 2 before reaching the
    // driver at all, and every rat command would do the same.
    let seen = painted_choose(&[], &[("RAT_APPEARANCE", "dark"), ("RAT_ACCESSIBLE", "")]);
    assert_frame_escapes_present(&seen, "a session with an empty variable");
}

#[test]
fn the_opening_block_names_the_header_the_items_the_position_and_the_keys() {
    let session = PtySession::spawn(
        &rat_bin(),
        &["choose", "--accessible", "alpha", "beta", "gamma", "delta"],
        &[("RAT_APPEARANCE", "dark")],
    )
    .expect("spawn rat choose under a pty");
    let mut terminal = FakeTerminal::dark();
    wait_for_in_order(
        &session,
        &mut terminal,
        &[
            b"Choose:\r\n",
            b"alpha\r\n",
            b"beta\r\n",
            b"gamma\r\n",
            b"delta\r\n",
            b"1 of 4\r\n",
            KEYS_ONE,
        ],
        Duration::from_secs(5),
    );
    session.write_bytes(b"\x1b");
    assert!(
        !session.kill_if_alive(Duration::from_secs(5)),
        "escape must exit"
    );
}

#[test]
fn the_opening_block_is_written_once_and_never_again() {
    let session = PtySession::spawn(
        &rat_bin(),
        &["choose", "--accessible", "alpha", "beta", "gamma", "delta"],
        &[("RAT_APPEARANCE", "dark")],
    )
    .expect("spawn rat choose under a pty");
    let mut terminal = FakeTerminal::dark();
    let opening = wait_for_in_order(
        &session,
        &mut terminal,
        &[b"Choose:\r\n", KEYS_ONE],
        Duration::from_secs(5),
    );
    assert_eq!(
        count(&opening, b"Choose:"),
        1,
        "the opening block is written once: {:?}",
        String::from_utf8_lossy(&opening)
    );

    // A keystroke must not re-open the block. The assertion names the
    // two rows only an opening can produce rather than demanding
    // silence, so it stays true once a keystroke starts speaking a
    // transition row of its own.
    session.write_bytes(b"\x1b[B");
    let after = drain_for(&session, Duration::from_millis(600));
    assert!(
        !contains(&after, b"Choose:"),
        "the header came back: {:?}",
        String::from_utf8_lossy(&after)
    );
    assert!(
        !contains(&after, KEYS_ONE),
        "the keys row came back: {:?}",
        String::from_utf8_lossy(&after)
    );

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(5));
}

#[test]
fn a_list_longer_than_the_cap_lists_its_head_and_says_how_many_there_are() {
    // Sixty rather than one past the cap: the cap is a starting value,
    // and a fixture sized to it would turn any future raise into a
    // mystifying red. If it is ever raised past 59, this fixture grows.
    let items: Vec<String> = (1..=60).map(|n| format!("opt{n:02}")).collect();
    let mut argv = vec!["choose", "--accessible"];
    argv.extend(items.iter().map(String::as_str));
    let session = PtySession::spawn(&rat_bin(), &argv, &[("RAT_APPEARANCE", "dark")])
        .expect("spawn rat choose under a pty");
    let mut terminal = FakeTerminal::dark();
    let opening = wait_for_in_order(
        &session,
        &mut terminal,
        &[b"Choose:\r\n", b"opt01\r\n", b"\r\nfirst ", b"1 of 60\r\n"],
        Duration::from_secs(5),
    );
    assert!(
        !contains(&opening, b"opt60\r\n"),
        "the cap must not list the tail: {:?}",
        String::from_utf8_lossy(&opening)
    );

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(5));
}

#[test]
fn an_item_carrying_a_newline_is_still_one_row() {
    let session = PtySession::spawn(
        &rat_bin(),
        &["choose", "--accessible", "a\nb", "beta"],
        &[("RAT_APPEARANCE", "dark")],
    )
    .expect("spawn rat choose under a pty");
    let mut terminal = FakeTerminal::dark();
    let opening = wait_for_in_order(
        &session,
        &mut terminal,
        &[b"Choose:\r\n", b"1 of 2\r\n"],
        Duration::from_secs(5),
    );
    // Two items, two rows — whatever the writer replaces the break with.
    // Counting terminators rather than matching text keeps this test
    // true for any replacement it chooses.
    let body = between(&opening, b"Choose:\r\n", b"1 of 2\r\n");
    assert_eq!(
        count(body, b"\r\n"),
        2,
        "an item must not forge a row: {:?}",
        String::from_utf8_lossy(body)
    );

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(5));
}

#[test]
fn a_cursor_move_names_the_item_and_a_toggle_names_the_mark() {
    let session = PtySession::spawn(
        &rat_bin(),
        &[
            "choose",
            "--accessible",
            "--no-limit",
            "alpha",
            "beta",
            "gamma",
            "delta",
        ],
        &[("RAT_APPEARANCE", "dark")],
    )
    .expect("spawn rat choose under a pty");
    let mut terminal = FakeTerminal::dark();
    let mut seen = wait_for_in_order(
        &session,
        &mut terminal,
        &[KEYS_MULTI],
        Duration::from_secs(5),
    );
    seen.extend(press_and_hear(
        &session,
        &mut terminal,
        b"\x1b[B",
        b"beta\r\n",
    ));
    seen.extend(press_and_hear(
        &session,
        &mut terminal,
        b" ",
        b"selected beta\r\n",
    ));
    seen.extend(press_and_hear(
        &session,
        &mut terminal,
        b" ",
        b"deselected beta\r\n",
    ));
    assert_eq!(
        common::pty::first_unmatched_in_order(
            &seen,
            &[
                KEYS_MULTI,
                b"beta\r\n",
                b"selected beta\r\n",
                b"deselected beta\r\n",
            ],
        ),
        None,
        "{:?}",
        String::from_utf8_lossy(&seen)
    );

    session.write_bytes(b"\x1b");
    assert!(!session.kill_if_alive(Duration::from_secs(5)));
}

#[test]
fn an_up_at_the_top_of_the_list_says_nothing() {
    let session = PtySession::spawn(
        &rat_bin(),
        &["choose", "--accessible", "alpha", "beta", "gamma", "delta"],
        &[("RAT_APPEARANCE", "dark")],
    )
    .expect("spawn rat choose under a pty");
    let mut terminal = FakeTerminal::dark();
    wait_for_in_order(&session, &mut terminal, &[KEYS_ONE], Duration::from_secs(5));

    session.write_bytes(b"\x1b[A");
    assert_silent_then_alive(&session, &mut terminal, (b"\x1b[B", b"beta\r\n"));

    session.write_bytes(b"\x1b");
    assert!(!session.kill_if_alive(Duration::from_secs(5)));
}

#[test]
fn a_down_at_the_bottom_of_the_list_says_nothing() {
    let session = PtySession::spawn(
        &rat_bin(),
        &["choose", "--accessible", "alpha", "beta", "gamma", "delta"],
        &[("RAT_APPEARANCE", "dark")],
    )
    .expect("spawn rat choose under a pty");
    let mut terminal = FakeTerminal::dark();
    wait_for_in_order(&session, &mut terminal, &[KEYS_ONE], Duration::from_secs(5));

    // Walked down rather than jumped with End: End reaches the wire as
    // one byte sequence on some terminals and another elsewhere, and
    // this suite has no precedent for either. Walking needs no such
    // assumption and arrives at the clamp the way a reader does.
    for row in [b"beta\r\n".as_slice(), b"gamma\r\n", b"delta\r\n"] {
        press_and_hear(&session, &mut terminal, b"\x1b[B", row);
    }

    session.write_bytes(b"\x1b[B");
    assert_silent_then_alive(&session, &mut terminal, (b"\x1b[A", b"gamma\r\n"));

    session.write_bytes(b"\x1b");
    assert!(!session.kill_if_alive(Duration::from_secs(5)));
}

#[test]
fn a_selection_the_limit_refuses_says_nothing() {
    let session = PtySession::spawn(
        &rat_bin(),
        &[
            "choose",
            "--accessible",
            "--limit",
            "2",
            "alpha",
            "beta",
            "gamma",
            "delta",
        ],
        &[("RAT_APPEARANCE", "dark")],
    )
    .expect("spawn rat choose under a pty");
    let mut terminal = FakeTerminal::dark();
    wait_for_in_order(
        &session,
        &mut terminal,
        &[KEYS_MULTI],
        Duration::from_secs(5),
    );

    press_and_hear(&session, &mut terminal, b" ", b"selected alpha\r\n");
    press_and_hear(&session, &mut terminal, b"\x1b[B", b"beta\r\n");
    press_and_hear(&session, &mut terminal, b" ", b"selected beta\r\n");
    press_and_hear(&session, &mut terminal, b"\x1b[B", b"gamma\r\n");

    // The limit is met, so the toggle takes its empty arm. There is no
    // vocabulary for a refusal, and inventing one here would invent it
    // in the one place nobody would look for it.
    session.write_bytes(b" ");
    assert_silent_then_alive(&session, &mut terminal, (b"\x1b[B", b"delta\r\n"));

    session.write_bytes(b"\x1b");
    assert!(!session.kill_if_alive(Duration::from_secs(5)));
}

#[test]
fn a_key_no_reducer_claims_says_nothing() {
    let session = PtySession::spawn(
        &rat_bin(),
        &["choose", "--accessible", "alpha", "beta", "gamma", "delta"],
        &[("RAT_APPEARANCE", "dark")],
    )
    .expect("spawn rat choose under a pty");
    let mut terminal = FakeTerminal::dark();
    wait_for_in_order(&session, &mut terminal, &[KEYS_ONE], Duration::from_secs(5));

    // The one arm where the reducer did nothing, rather than doing
    // something that happened to land where it started: the mode is
    // silent because the state did not change, not because the driver
    // keeps a list of keys it answers.
    session.write_bytes(b"x");
    assert_silent_then_alive(&session, &mut terminal, (b"\x1b[B", b"beta\r\n"));

    session.write_bytes(b"\x1b");
    assert!(!session.kill_if_alive(Duration::from_secs(5)));
}

#[test]
fn the_closing_row_and_stdout_name_the_same_items_in_the_same_order() {
    let session = PtySession::spawn(
        &rat_bin(),
        &[
            "choose",
            "--accessible",
            "--no-limit",
            "alpha",
            "beta",
            "gamma",
            "delta",
        ],
        &[("RAT_APPEARANCE", "dark")],
    )
    .expect("spawn rat choose under a pty");
    let mut terminal = FakeTerminal::dark();
    wait_for_in_order(
        &session,
        &mut terminal,
        &[KEYS_MULTI],
        Duration::from_secs(5),
    );

    press_and_hear(&session, &mut terminal, b"\x1b[B", b"beta\r\n");
    press_and_hear(&session, &mut terminal, b" ", b"selected beta\r\n");
    press_and_hear(&session, &mut terminal, b"\x1b[A", b"alpha\r\n");
    press_and_hear(&session, &mut terminal, b" ", b"selected alpha\r\n");

    session.write_bytes(b"\r");
    let tail = drain_for(&session, Duration::from_secs(1));
    assert_eq!(
        common::pty::first_unmatched_in_order(
            &tail,
            &[b"chose beta, alpha\r\n", b"beta\r\nalpha\r\n"]
        ),
        None,
        "the transcript and stdout must agree, in order: {:?}",
        String::from_utf8_lossy(&tail),
    );

    session.kill_if_alive(Duration::from_secs(5));
}

#[test]
fn an_empty_submit_says_chose_nothing_and_prints_nothing() {
    // `--no-limit` is load-bearing: under the default single-select
    // limit, enter toggles the cursor item on the way out, so an empty
    // submit cannot happen at all.
    let session = PtySession::spawn(
        &rat_bin(),
        &[
            "choose",
            "--accessible",
            "--no-limit",
            "alpha",
            "beta",
            "gamma",
            "delta",
        ],
        &[("RAT_APPEARANCE", "dark")],
    )
    .expect("spawn rat choose under a pty");
    let mut terminal = FakeTerminal::dark();
    wait_for_in_order(
        &session,
        &mut terminal,
        &[KEYS_MULTI],
        Duration::from_secs(5),
    );

    session.write_bytes(b"\r");
    let tail = drain_for(&session, Duration::from_secs(1));
    assert!(
        contains(&tail, b"chose nothing\r\n"),
        "an empty submit must still say so: {:?}",
        String::from_utf8_lossy(&tail)
    );
    // Nothing is printed for an empty result, so the closing row is the
    // last thing the session writes.
    assert!(
        after(&tail, b"chose nothing\r\n").is_empty(),
        "stdout must stay empty: {:?}",
        String::from_utf8_lossy(after(&tail, b"chose nothing\r\n"))
    );

    session.kill_if_alive(Duration::from_secs(5));
}

#[test]
fn escape_says_cancelled() {
    let session = PtySession::spawn(
        &rat_bin(),
        &["choose", "--accessible", "alpha", "beta", "gamma", "delta"],
        &[("RAT_APPEARANCE", "dark")],
    )
    .expect("spawn rat choose under a pty");
    let mut terminal = FakeTerminal::dark();
    wait_for_in_order(&session, &mut terminal, &[KEYS_ONE], Duration::from_secs(5));

    session.write_bytes(b"\x1b");
    let tail = drain_for(&session, Duration::from_secs(1));
    assert!(
        contains(&tail, b"cancelled\r\n"),
        "escape must say so: {:?}",
        String::from_utf8_lossy(&tail)
    );
    assert!(after(&tail, b"cancelled\r\n").is_empty());
    assert!(!session.kill_if_alive(Duration::from_secs(5)));
}

#[test]
fn ctrl_c_says_cancelled_the_same_way_escape_does() {
    // A separate session from the escape arm on purpose: the two paths
    // diverge inside the driver, and a shared session could not say
    // which one broke.
    let session = PtySession::spawn(
        &rat_bin(),
        &["choose", "--accessible", "alpha", "beta", "gamma", "delta"],
        &[("RAT_APPEARANCE", "dark")],
    )
    .expect("spawn rat choose under a pty");
    let mut terminal = FakeTerminal::dark();
    wait_for_in_order(&session, &mut terminal, &[KEYS_ONE], Duration::from_secs(5));

    session.write_bytes(b"\x03");
    let tail = drain_for(&session, Duration::from_secs(1));
    assert!(
        contains(&tail, b"cancelled\r\n"),
        "raw mode makes this a keystroke rather than a signal, so the row \
         is still written; a timeout here means the terminal kept its \
         signal handling: {:?}",
        String::from_utf8_lossy(&tail)
    );
    assert!(!session.kill_if_alive(Duration::from_secs(5)));
}

#[test]
fn a_timeout_says_timed_out() {
    let session = PtySession::spawn(
        &rat_bin(),
        &[
            "choose",
            "--accessible",
            "--timeout",
            "300ms",
            "alpha",
            "beta",
            "gamma",
            "delta",
        ],
        &[("RAT_APPEARANCE", "dark")],
    )
    .expect("spawn rat choose under a pty");
    let mut terminal = FakeTerminal::dark();
    wait_for_in_order(&session, &mut terminal, &[KEYS_ONE], Duration::from_secs(5));

    let tail = drain_for(&session, SILENCE_WINDOW);
    // Two writers say this, and they are byte-identical: the transcript
    // row (raw mode, explicit terminator) and the shipped give-up line
    // on stderr (after raw mode is off, so the terminal adds the return).
    // Exactly two — one is the transcript missing, three is somebody
    // writing it twice.
    assert_eq!(
        count(&tail, b"timed out\r\n"),
        2,
        "the transcript row and the stderr line must both be there: {:?}",
        String::from_utf8_lossy(&tail),
    );

    session.kill_if_alive(Duration::from_secs(5));
}

#[test]
fn a_move_inside_the_debounce_window_is_dropped_by_the_key_that_ends_it() {
    let session = PtySession::spawn(
        &rat_bin(),
        &["choose", "--accessible", "alpha", "beta", "gamma", "delta"],
        &[("RAT_APPEARANCE", "dark")],
    )
    .expect("spawn rat choose under a pty");
    let mut terminal = FakeTerminal::dark();
    wait_for_in_order(&session, &mut terminal, &[KEYS_ONE], Duration::from_secs(5));

    // Down and enter back to back, inside one quiescence interval. The
    // pending row is discarded at exit and only the closing row is
    // written: the resting state at exit IS the result. This is the
    // burst policy working as designed — do not "fix" it.
    session.write_bytes(b"\x1b[B\r");
    let tail = drain_for(&session, SILENCE_WINDOW);
    assert!(
        contains(&tail, b"chose beta\r\n"),
        "the closing row must arrive: {:?}",
        String::from_utf8_lossy(&tail)
    );
    let middle = before(&tail, b"chose beta\r\n");
    assert!(
        middle.is_empty(),
        "a row pending at exit must be discarded, not flushed: {:?}",
        String::from_utf8_lossy(middle),
    );

    session.kill_if_alive(Duration::from_secs(5));
}

#[test]
fn an_answer_never_arrives_before_the_row_it_answers_about() {
    let session = PtySession::spawn(
        &rat_bin(),
        &["choose", "--accessible", "alpha", "beta", "gamma", "delta"],
        &[("RAT_APPEARANCE", "dark")],
    )
    .expect("spawn rat choose under a pty");
    let mut terminal = FakeTerminal::dark();
    wait_for_in_order(&session, &mut terminal, &[KEYS_ONE], Duration::from_secs(5));

    // ONE write, deliberately: the second key must land inside the
    // quiescence window while the first key's row is still pending, and
    // that pending row is the only state this test can measure. Reading
    // the first row back before sending the second — the rule everywhere
    // else in this suite — would destroy it and the test would pass
    // against the bug.
    //
    // The first needle carries its terminator so `beta` cannot match
    // inside `beta 2 of 4` and satisfy the chain backwards.
    session.write_bytes(b"\x1b[B\x0f");
    wait_for_in_order(
        &session,
        &mut terminal,
        &[b"beta\r\n", b"beta 2 of 4\r\n"],
        Duration::from_secs(5),
    );

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(5));
}

/// Spawn, hear the whole opening block, leave, and hand back everything
/// the session wrote.
fn transcript(args: &[&str]) -> Vec<u8> {
    let mut argv = vec!["choose", "--accessible"];
    argv.extend_from_slice(args);
    let session =
        PtySession::spawn(&rat_bin(), &argv, &[("RAT_APPEARANCE", "dark")]).expect("spawn");
    let mut terminal = FakeTerminal::dark();
    let mut seen = wait_for_in_order(
        &session,
        &mut terminal,
        &[b"escape cancels"],
        Duration::from_secs(5),
    );
    session.write_bytes(b"\x1b");
    seen.extend(drain_for(&session, Duration::from_millis(600)));
    session.kill_if_alive(Duration::from_secs(5));
    seen
}

#[test]
fn the_keys_row_names_the_on_demand_keys_once() {
    // The clause is these keys' only advertisement, and the keys row is
    // written once — so is the clause. A count, not a containment check:
    // the failure worth catching is a SECOND copy, which containment
    // passes.
    let multi = transcript(&["--no-limit", "alpha", "beta", "gamma", "delta"]);
    assert_eq!(count(&multi, b"control o says where you are"), 1);
    assert_eq!(count(&multi, b"control t says what you selected"), 1);

    // A single selection advertises only the first: with one selection
    // the tagged-set key answers the zero member almost every press.
    let single = transcript(&["alpha", "beta", "gamma", "delta"]);
    assert_eq!(count(&single, b"control o says where you are"), 1);
    assert_eq!(count(&single, b"control t says what you selected"), 0);
}

/// The exit code, or a diagnostic naming which of the three ways it went
/// wrong: still running, or dead of a signal. All three collapse to
/// `None`, and they want different fixes.
fn exit_code(session: &PtySession) -> i32 {
    match session.wait_code(Duration::from_secs(5)) {
        Some(code) => code,
        None => panic!(
            "no exit code: exited={}, so it is either still running or died of a signal",
            session.exited()
        ),
    }
}

#[test]
fn a_clean_exit_reports_its_code() {
    let session = PtySession::spawn(
        &rat_bin(),
        &["choose", "--accessible=true", "alpha", "beta"],
        &[("RAT_APPEARANCE", "dark")],
    )
    .expect("spawn rat choose under a pty");
    let mut terminal = FakeTerminal::dark();
    wait_for_in_order(&session, &mut terminal, &[KEYS_ONE], Duration::from_secs(5));

    session.write_bytes(b"\x1b");
    assert_eq!(exit_code(&session), 1);
}

#[test]
fn a_reaped_session_does_not_wedge_the_killer() {
    let session = PtySession::spawn(
        &rat_bin(),
        &["choose", "--accessible", "alpha", "beta"],
        &[("RAT_APPEARANCE", "dark")],
    )
    .expect("spawn rat choose under a pty");
    let mut terminal = FakeTerminal::dark();
    wait_for_in_order(&session, &mut terminal, &[KEYS_ONE], Duration::from_secs(5));
    session.write_bytes(b"\x1b");

    // `exited()` reaps. Asking the killer afterwards used to wait for a
    // reap that had already happened, with no way to tell that from a
    // live child.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !session.exited() {
        assert!(std::time::Instant::now() < deadline, "the child never left");
        let _ = drain_for(&session, Duration::from_millis(20));
    }
    assert!(
        !session.kill_if_alive(Duration::from_secs(2)),
        "the child had already exited"
    );
    // And the status survived a reap that happened somewhere else
    // entirely, which is the whole reason it is recorded rather than
    // read on demand.
    assert_eq!(session.wait_code(Duration::from_secs(1)), Some(1));
}

/// One fixture for both modes. The transcript arm adds the variable; the
/// painted arm is the shipped default, and its only "first output" is a
/// frame escape, because it has no rows at all — which is the measured
/// difference this whole mode exists to remove, not a gap in the test.
fn spawn_mode(args: &[&str], transcript: bool) -> PtySession {
    let mut argv = vec!["choose"];
    argv.extend_from_slice(args);
    let mut envs = vec![("RAT_APPEARANCE", "dark")];
    if transcript {
        envs.push(("RAT_ACCESSIBLE", "1"));
    }
    PtySession::spawn(&rat_bin(), &argv, &envs).expect("spawn rat choose under a pty")
}

/// The needle that proves the session reached its driver — and therefore
/// that raw mode is on, which is what makes an interrupt byte a keystroke
/// rather than a signal.
fn first_output(transcript: bool) -> &'static [u8] {
    if transcript { KEYS_ONE } else { b"\x1b[?25l" }
}

#[test]
fn escape_exits_one_in_both_modes() {
    for transcript in [true, false] {
        let session = spawn_mode(&["alpha", "beta", "gamma", "delta"], transcript);
        let mut terminal = FakeTerminal::dark();
        wait_for_in_order(
            &session,
            &mut terminal,
            &[first_output(transcript)],
            Duration::from_secs(5),
        );
        if transcript {
            press_and_hear(&session, &mut terminal, b"\x1b", b"cancelled\r\n");
        } else {
            session.write_bytes(b"\x1b");
        }
        assert_eq!(exit_code(&session), 1, "transcript={transcript}");
    }
}

#[test]
fn an_interrupt_exits_one_hundred_and_thirty_in_both_modes() {
    for transcript in [true, false] {
        let session = spawn_mode(&["alpha", "beta", "gamma", "delta"], transcript);
        let mut terminal = FakeTerminal::dark();
        // Waiting for the session's own first output IS the proof that
        // raw mode is engaged: before the guard the line discipline still
        // owns the interrupt byte and would send a signal instead, and
        // the child would die of it rather than exiting. A sleep would
        // work most of the time and fail on a loaded runner, and the
        // failure would look like a flaky exit code rather than a race.
        wait_for_in_order(
            &session,
            &mut terminal,
            &[first_output(transcript)],
            Duration::from_secs(5),
        );
        if transcript {
            press_and_hear(&session, &mut terminal, b"\x03", b"cancelled\r\n");
        } else {
            session.write_bytes(b"\x03");
        }
        assert_eq!(exit_code(&session), 130, "transcript={transcript}");
    }
}

#[test]
fn a_timeout_exits_one_hundred_and_twenty_four_in_both_modes() {
    for transcript in [true, false] {
        let session = spawn_mode(&["--timeout", "300ms", "alpha", "beta"], transcript);
        // The code, and NOTHING about the stream. The give-up notice is
        // printed to stderr on this same pty after raw mode drops, in
        // bytes identical to the transcript's own row — so a containment
        // check here could not fail. The row is asserted by count where
        // its wording lives.
        assert_eq!(exit_code(&session), 124, "transcript={transcript}");
    }
}

#[test]
fn the_render_flags_have_nothing_to_act_on() {
    // The same session shape twice, with no keystroke, so the comparison
    // is deterministic: the burst policy makes a keystroke's row boundary
    // a function of timing, and the opening block is where all three of
    // these flags would show up if any of them were live.
    //
    // A painted run would draw three of four under the height flag; a
    // transcript has no frame to fit.
    let plain = transcript(&["alpha", "beta", "gamma", "delta"]);
    let flagged = transcript(&[
        "--cursor",
        ">>>",
        "--selected-prefix",
        "X",
        "--height",
        "3",
        "alpha",
        "beta",
        "gamma",
        "delta",
    ]);
    assert_eq!(
        String::from_utf8_lossy(&plain),
        String::from_utf8_lossy(&flagged)
    );
    assert!(
        !contains(&flagged, b">>>"),
        "the cursor marker has no row to sit on"
    );
    // Byte-identity alone would pass on two equally truncated runs.
    for item in [&b"alpha\r\n"[..], b"beta\r\n", b"gamma\r\n", b"delta\r\n"] {
        assert!(
            contains(&flagged, item),
            "all four are named whatever the height says: {:?}",
            String::from_utf8_lossy(&flagged)
        );
    }
}

#[test]
fn a_toggle_row_never_wears_the_selected_prefix() {
    let session = PtySession::spawn(
        &rat_bin(),
        &[
            "choose",
            "--accessible",
            "--no-limit",
            "--cursor",
            ">>>",
            "--selected-prefix",
            "X",
            "alpha",
            "beta",
            "gamma",
        ],
        &[("RAT_APPEARANCE", "dark")],
    )
    .expect("spawn rat choose under a pty");
    let mut terminal = FakeTerminal::dark();
    let mut seen = wait_for_in_order(
        &session,
        &mut terminal,
        &[KEYS_MULTI],
        Duration::from_secs(5),
    );
    seen.extend(press_and_hear(
        &session,
        &mut terminal,
        b"\x1b[B",
        b"beta\r\n",
    ));
    seen.extend(press_and_hear(
        &session,
        &mut terminal,
        b" ",
        b"selected beta\r\n",
    ));
    // Both spellings: the painted render joins the prefix without a
    // separator, and a transcript that reached for the render fields
    // would produce either.
    for forged in [&b"Xbeta"[..], b"X beta"] {
        assert!(
            !contains(&seen, forged),
            "a render prefix reached a row: {:?}",
            String::from_utf8_lossy(&seen)
        );
    }

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(5));
}

/// Drive a session whose stdout is a file rather than the terminal — the
/// command-substitution shape, run under a terminal, which is where a
/// user actually is. The transcript still lands on the pty, so the
/// captured stream is pure transcript and the file is pure result.
///
/// `/bin/sh` by absolute path: the spawned child's environment is built
/// from scratch and carries no search path, so nothing here may be found
/// by name.
fn run_capturing_stdout(envs: &[(&str, &str)], out: &std::path::Path) -> String {
    let script = format!(
        "{} choose alpha beta gamma delta > {}",
        rat_bin(),
        out.display()
    );
    let mut all = vec![("RAT_APPEARANCE", "dark")];
    all.extend_from_slice(envs);
    let session =
        PtySession::spawn("/bin/sh", &["-c", &script], &all).expect("spawn a shell under a pty");
    let mut terminal = FakeTerminal::dark();
    let transcript = envs.iter().any(|(k, _)| *k == "RAT_ACCESSIBLE");
    if transcript {
        wait_for_in_order(&session, &mut terminal, &[KEYS_ONE], Duration::from_secs(5));
        press_and_hear(&session, &mut terminal, b"\x1b[B", b"beta\r\n");
    } else {
        wait_for_in_order(
            &session,
            &mut terminal,
            &[b"\x1b[?25l"],
            Duration::from_secs(5),
        );
        session.write_bytes(b"\x1b[B");
    }
    session.write_bytes(b"\r");
    assert_eq!(session.wait_code(Duration::from_secs(5)), Some(0));
    std::fs::read_to_string(out).expect("the redirect target exists")
}

#[test]
fn the_result_reaches_a_command_substitution_in_both_modes() {
    // The transcript rides the terminal directly and the result rides
    // standard output, so the two never touch — and this is the arm that
    // fails loudly if a row is ever written to the wrong one.
    let dir = std::env::temp_dir().join(format!("rat-stdout-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a place for the two redirects");
    let painted_at = dir.join("painted");
    let spoken_at = dir.join("spoken");

    let painted = run_capturing_stdout(&[], &painted_at);
    let spoken = run_capturing_stdout(&[("RAT_ACCESSIBLE", "1")], &spoken_at);
    assert_eq!(painted, "beta\n");
    assert_eq!(spoken, painted, "the mode must not move standard output");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The three queries the startup appearance detection writes, matched as
/// prefixes: the terminator is either a bell or a string terminator
/// depending on how the library was built, and pinning the one we happen
/// to observe would make an upstream detail a contract.
const STARTUP_QUERIES: [&[u8]; 3] = [b"\x1b]11", b"\x1b]10", b"\x1b[c"];

/// Spawn at an explicitly automatic appearance and capture everything up
/// to the end of the session.
///
/// NOT `RAT_APPEARANCE=dark`, unlike every other test in this suite:
/// pinning it turns the appearance mode away from automatic, and the
/// absence below would then be true for a reason that has nothing to do
/// with this mode. The automatic setting is passed explicitly for the
/// same reason — the default is allowed to move.
///
/// A terminal is driven even though the spoken arm should never query
/// it: if the suppression breaks, the query fires, nobody answers, and
/// the child stalls for the probe's whole timeout — turning a clean
/// failure into a slow one. Both arms share the fixture, because two
/// arms that differ in their terminal are not a pair.
fn appearance_probe_stream(spoken: bool) -> Vec<u8> {
    let envs: Vec<(&str, &str)> = if spoken {
        vec![("RAT_ACCESSIBLE", "1")]
    } else {
        Vec::new()
    };
    let session = PtySession::spawn(
        &rat_bin(),
        &["--appearance", "auto", "choose", "alpha", "beta"],
        &envs,
    )
    .expect("spawn rat choose under a pty");
    let mut terminal = FakeTerminal::dark();
    let mut seen = drain_for(&session, Duration::from_millis(900));
    session.write_bytes(b"\x1b");
    seen.extend(drain_for(&session, Duration::from_millis(600)));
    let _ = &mut terminal;
    session.kill_if_alive(Duration::from_secs(5));
    seen
}

#[test]
fn a_spoken_session_never_asks_the_terminal_what_colour_it_is() {
    let seen = appearance_probe_stream(true);
    for query in STARTUP_QUERIES {
        assert!(
            !contains(&seen, query),
            "the transcript asked the terminal a question: {:?} in {:?}",
            String::from_utf8_lossy(query),
            String::from_utf8_lossy(&seen)
        );
    }
    // Liveness, so the absence is not simply a session that never ran.
    assert!(
        contains(&seen, b"Choose:\r\n"),
        "{:?}",
        String::from_utf8_lossy(&seen)
    );
}

#[test]
fn the_painted_path_still_asks() {
    // The positive control. Under a terminal the child is a session
    // leader in the foreground group, so the query's own guard passes and
    // it really does write — which is what makes the absence above mean
    // the mode suppressed it, rather than the process dying early, the
    // guard refusing, or the needle being wrong.
    let seen = appearance_probe_stream(false);
    for query in STARTUP_QUERIES {
        assert!(
            contains(&seen, query),
            "the painted path must still ask: {:?}",
            String::from_utf8_lossy(query)
        );
    }
}
