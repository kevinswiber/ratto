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
const KEYS_ONE: &[u8] = b"up and down move, enter chooses, escape cancels\r\n";
const KEYS_MULTI: &[u8] = b"up and down move, space selects, enter confirms, escape cancels\r\n";

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
