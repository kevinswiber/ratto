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
