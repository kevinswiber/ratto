#![cfg(unix)]
mod common;

use std::time::Duration;

use common::pty::{
    ECHO_FORBIDDEN, FakeTerminal, PtySession, assert_no_echo_escapes, drain_for,
    try_wait_for_in_order, wait_for_in_order,
};

/// Path to the rat binary — mirrors `tests/pty_watch.rs`'s local
/// `rat_bin()` per that file's precedent.
fn rat_bin() -> String {
    assert_cmd::cargo::cargo_bin("rat").display().to_string()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.len() <= haystack.len() && haystack.windows(needle.len()).any(|w| w == needle)
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.len() > haystack.len() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// Everything ahead of the first occurrence of `needle`.
fn before<'a>(haystack: &'a [u8], needle: &[u8]) -> &'a [u8] {
    let at = haystack
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("the needle must be present");
    &haystack[..at]
}

/// Everything after the first occurrence of `needle`.
fn after<'a>(haystack: &'a [u8], needle: &[u8]) -> &'a [u8] {
    let at = haystack
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("the needle must be present");
    &haystack[at + needle.len()..]
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

/// The opening block's last row for a single-select run. Every
/// post-opening assertion in this suite anchors on it: it is the only
/// row that means "the opening is over", and it carries its own row
/// terminator so a capture stopped here leaves no `\r\n` behind for a
/// later drain to read back as a phantom row.
/// How long a silence proof must listen before it may believe the
/// silence. A row noted into the burst policy is written one quiescence
/// interval after the last input, and the driver notices it is due on
/// its next turn of the poll loop — so the worst case a pending row can
/// hide in is one quiescence plus one poll slice. Both are 250 ms today
/// and the second is DEFINED as the first (in `src/ui/loop_.rs`), which
/// makes the floor 500 ms; this doubles it for a loaded CI box.
///
/// There is no library target to import those constants from, so the
/// relation lives here and must move when they do. A drain shorter than
/// the sum does not measure silence — it measures a row still in flight.
const SILENCE_WINDOW: Duration = Duration::from_millis(1000);

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

/// One key, one row. In echo mode a burst of keys COALESCES to a single
/// row — the last one — so an ordered chain of transitions cannot be
/// produced by writing several keys and then reading. Each key is
/// written only after the previous key's row has been read back, which
/// is also what makes the sequence race-free: the next row cannot
/// already be in flight inside the previous wait's final chunk, because
/// nothing has been pressed yet to cause it.
///
/// Returns the bytes it read so the caller can concatenate them into
/// ONE stream and assert the whole order at the end. Reads are
/// destructive and sequential, so the concatenation is the stream.
fn press_and_hear(
    session: &PtySession,
    terminal: &mut FakeTerminal,
    key: &[u8],
    row: &[u8],
) -> Vec<u8> {
    session.write_bytes(key);
    wait_for_in_order(session, terminal, &[row], Duration::from_secs(5))
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

/// Beats 3 and 4 of a silence proof: listen long enough to believe the
/// silence, then prove the session could still have spoken.
fn assert_silent_then_alive(
    session: &PtySession,
    terminal: &mut FakeTerminal,
    then: (&[u8], &[u8]),
) {
    let quiet = drain_for(session, SILENCE_WINDOW);
    assert!(
        quiet.is_empty(),
        "a key that changed nothing wrote {:?}",
        String::from_utf8_lossy(&quiet),
    );
    // An empty drain and a dead process look identical. This is the
    // difference.
    press_and_hear(session, terminal, then.0, then.1);
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
