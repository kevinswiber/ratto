#![cfg(unix)]
mod common;

use std::time::Duration;

use common::pty::{FakeTerminal, PtySession, assert_no_echo_escapes, drain_for, wait_for_in_order};

/// Path to the rat binary — mirrors `tests/pty_watch.rs`'s local
/// `rat_bin()` per that file's precedent.
fn rat_bin() -> String {
    assert_cmd::cargo::cargo_bin("rat").display().to_string()
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
