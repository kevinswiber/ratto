#![cfg(unix)]
mod common;

use std::time::Duration;

use common::pty::{
    FakeTerminal, PtySession, drain_for, first_unmatched_in_order, wait_for_in_order,
};

fn rat_bin() -> String {
    assert_cmd::cargo::cargo_bin("rat").display().to_string()
}

/// The block's last row. Named once, because every test in this suite
/// anchors on it and because it must stay in step with what the surface
/// says.
///
/// Anchoring on the LAST row is what makes a later `No\r\n` needle
/// unambiguous: confirm's transition rows are byte-identical to its
/// opening rows, so a wait that has heard the keys row has necessarily
/// consumed the whole block and left nothing behind to satisfy one.
const KEYS_ROW: &[u8] = b"left and right move, y and n answer, enter chooses, escape cancels\r\n";

/// One quiescence interval plus one poll slice, plus margin for a
/// loaded CI box. There is no library target to import those constants
/// from, so the relation lives here and must move when they do. A
/// shorter drain does not measure silence — it measures a row still in
/// flight.
const SILENCE_WINDOW: Duration = Duration::from_millis(1000);

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.len() <= haystack.len() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Press one key and hear the row it causes, before anything else is
/// pressed. Race-free by construction: the next row cannot already be
/// in flight inside this wait's final chunk, because nothing has been
/// pressed yet to cause it. Returns the bytes read so a caller can
/// concatenate them into ONE stream and assert the whole order at the
/// end — reads are destructive and sequential, so the concatenation
/// IS the stream.
fn press_and_hear(
    session: &PtySession,
    terminal: &mut FakeTerminal,
    key: &[u8],
    row: &[u8],
) -> Vec<u8> {
    session.write_bytes(key);
    wait_for_in_order(session, terminal, &[row], Duration::from_secs(5))
}

/// Listen long enough to believe the silence, then prove the session
/// could still have spoken. An empty drain and a dead process are
/// indistinguishable; this is the difference.
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
    press_and_hear(session, terminal, then.0, then.1);
}

fn spawn(args: &[&str]) -> PtySession {
    // A pinned appearance suppresses the startup probe, so every byte in
    // the stream is the transcript's own.
    PtySession::spawn(&rat_bin(), args, &[("RAT_APPEARANCE", "dark")])
        .expect("spawn rat confirm under a pty")
}

#[test]
fn the_opening_block_names_the_question_both_answers_and_the_armed_one() {
    let session = spawn(&["confirm", "Ship it?", "--accessible"]);
    let mut terminal = FakeTerminal::dark();
    wait_for_in_order(
        &session,
        &mut terminal,
        &[b"Ship it?\r\n", b"Yes\r\n", b"No\r\n", b"Yes\r\n", KEYS_ROW],
        Duration::from_secs(5),
    );

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(5));
}

#[test]
fn a_toggle_speaks_the_answer_it_moved_to() {
    let session = spawn(&["confirm", "Ship it?", "--accessible"]);
    let mut terminal = FakeTerminal::dark();
    let mut seen = wait_for_in_order(&session, &mut terminal, &[KEYS_ROW], Duration::from_secs(5));
    seen.extend(press_and_hear(&session, &mut terminal, b"\t", b"No\r\n"));
    seen.extend(press_and_hear(
        &session,
        &mut terminal,
        b"\x1b[D",
        b"Yes\r\n",
    ));
    // The aliases must say what the arrows say: a transcript that
    // derived its row from the KEY rather than from the state would
    // answer differently here.
    seen.extend(press_and_hear(&session, &mut terminal, b"l", b"No\r\n"));
    seen.extend(press_and_hear(&session, &mut terminal, b"h", b"Yes\r\n"));
    assert_eq!(
        first_unmatched_in_order(
            &seen,
            &[KEYS_ROW, b"No\r\n", b"Yes\r\n", b"No\r\n", b"Yes\r\n"],
        ),
        None,
        "{:?}",
        String::from_utf8_lossy(&seen)
    );

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(5));
}

#[test]
fn a_key_that_changes_nothing_says_nothing() {
    let session = spawn(&["confirm", "Ship it?", "--accessible"]);
    let mut terminal = FakeTerminal::dark();
    wait_for_in_order(&session, &mut terminal, &[KEYS_ROW], Duration::from_secs(5));

    // Left SETS the affirmative rather than moving, and it is already
    // armed — so the state does not change and neither does the
    // transcript.
    session.write_bytes(b"\x1b[D");
    assert_silent_then_alive(&session, &mut terminal, (b"\t", b"No\r\n"));

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(5));
}

#[test]
fn the_labels_are_what_the_transcript_speaks() {
    let session = spawn(&[
        "confirm",
        "Deploy?",
        "--affirmative",
        "Ship",
        "--negative",
        "Wait",
        "--default",
        "false",
        "--accessible",
    ]);
    let mut terminal = FakeTerminal::dark();
    wait_for_in_order(
        &session,
        &mut terminal,
        &[
            b"Deploy?\r\n",
            b"Ship\r\n",
            b"Wait\r\n",
            b"Wait\r\n",
            KEYS_ROW,
        ],
        Duration::from_secs(5),
    );
    press_and_hear(&session, &mut terminal, b"\t", b"Ship\r\n");

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(5));
}

#[test]
fn a_label_that_carries_a_newline_is_still_one_row() {
    // A real newline in the argv element: execve passes it verbatim and
    // no shell is involved.
    let session = spawn(&[
        "confirm",
        "Ship it?",
        "--affirmative",
        "ship\nit",
        "--accessible",
    ]);
    let mut terminal = FakeTerminal::dark();
    let block = wait_for_in_order(&session, &mut terminal, &[KEYS_ROW], Duration::from_secs(5));

    // Which character replaces the break is the writer's decision; that
    // a label cannot forge a row is this test's contract.
    assert_eq!(
        block
            .split(|&b| b == b'\n')
            .filter(|s| !s.is_empty())
            .count(),
        5,
        "the block must stay five rows: {:?}",
        String::from_utf8_lossy(&block)
    );
    assert!(
        !contains(&block, b"\r\nit\r\n"),
        "a label must not invent a row of its own: {:?}",
        String::from_utf8_lossy(&block)
    );
    assert!(
        block
            .split(|&b| b == b'\n')
            .any(|row| contains(row, b"ship") && contains(row, b"it")),
        "both halves of the label belong to one row: {:?}",
        String::from_utf8_lossy(&block)
    );

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(5));
}
