#![cfg(unix)]
mod common;

use std::time::Duration;

use common::pty::{
    FakeTerminal, PtySession, SILENCE_WINDOW, after, assert_silent_then_alive, before, contains,
    count, drain_for, first_unmatched_in_order, press_and_hear, wait_for_in_order,
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
const KEYS_ROW: &[u8] =
    b"left and right move, y and n answer, enter chooses, escape cancels, control o says where you are\r\n";

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

/// Anchor past the opening block, answer, and hand back everything the
/// session wrote after that. The drain is long enough that a wrongly
/// flushed row would have arrived.
fn answer_and_drain(session: &PtySession, terminal: &mut FakeTerminal, key: &[u8]) -> Vec<u8> {
    wait_for_in_order(session, terminal, &[KEYS_ROW], Duration::from_secs(5));
    session.write_bytes(key);
    drain_for(session, SILENCE_WINDOW)
}

#[test]
fn answering_yes_outright_speaks_the_choice_and_not_the_move() {
    // `--default false` is what gives this teeth: with the affirmative
    // already armed, `y` changes nothing and a missing move row would
    // prove only that the silence rule works. Starting from the
    // negative, the state really moves — so the move row's absence can
    // only be explained by the outcome being matched before the
    // transcript is asked what changed.
    let session = spawn(&["confirm", "Ship it?", "--default", "false", "--accessible"]);
    let mut terminal = FakeTerminal::dark();
    let tail = answer_and_drain(&session, &mut terminal, b"y");

    assert!(
        contains(&tail, b"chose Yes\r\n"),
        "the answer must be spoken: {:?}",
        String::from_utf8_lossy(&tail)
    );
    assert!(
        before(&tail, b"chose Yes\r\n").is_empty(),
        "no stuttered move row before the answer: {:?}",
        String::from_utf8_lossy(before(&tail, b"chose Yes\r\n"))
    );
    assert!(
        after(&tail, b"chose Yes\r\n").is_empty(),
        "nothing follows it: {:?}",
        String::from_utf8_lossy(after(&tail, b"chose Yes\r\n"))
    );

    session.kill_if_alive(Duration::from_secs(5));
}

#[test]
fn the_choice_is_spoken_when_stdout_says_nothing() {
    // Without the output flag this run prints not one byte and reports
    // its answer only through its exit code, so this row is the only
    // thing that says what was answered.
    let session = spawn(&["confirm", "Ship it?", "--accessible"]);
    let mut terminal = FakeTerminal::dark();
    let tail = answer_and_drain(&session, &mut terminal, b"\r");

    assert!(
        contains(&tail, b"chose Yes\r\n"),
        "the answer must be spoken even with nothing printed: {:?}",
        String::from_utf8_lossy(&tail)
    );
    assert!(before(&tail, b"chose Yes\r\n").is_empty());
    assert!(
        after(&tail, b"chose Yes\r\n").is_empty(),
        "this run prints nothing at all: {:?}",
        String::from_utf8_lossy(after(&tail, b"chose Yes\r\n"))
    );

    session.kill_if_alive(Duration::from_secs(5));
}

#[test]
fn showing_the_output_adds_a_printed_line_and_changes_no_row() {
    // Two writers on one device, told apart by content: the transcript's
    // closing row is written inside the driver, and the printed line
    // after it. Their agreement is the shared accessor made visible.
    let session = spawn(&["confirm", "Ship it?", "--show-output", "--accessible"]);
    let mut terminal = FakeTerminal::dark();
    let tail = answer_and_drain(&session, &mut terminal, b"\r");

    assert!(
        contains(&tail, b"chose Yes\r\n"),
        "{:?}",
        String::from_utf8_lossy(&tail)
    );
    assert!(before(&tail, b"chose Yes\r\n").is_empty());
    assert_eq!(
        after(&tail, b"chose Yes\r\n"),
        b"Yes\r\n",
        "the printed line is added, not substituted: {:?}",
        String::from_utf8_lossy(after(&tail, b"chose Yes\r\n"))
    );

    session.kill_if_alive(Duration::from_secs(5));
}

#[test]
fn answering_no_speaks_the_negative_label_not_a_cancel() {
    // A negative answer submits like an affirmative one; only the exit
    // code differs. Reading "not ok" as a cancel would put the wrong row
    // on half of all prompts.
    let session = spawn(&["confirm", "Ship it?", "--show-output", "--accessible"]);
    let mut terminal = FakeTerminal::dark();
    let tail = answer_and_drain(&session, &mut terminal, b"n");

    assert!(
        contains(&tail, b"chose No\r\n"),
        "a negative answer is still an answer: {:?}",
        String::from_utf8_lossy(&tail)
    );
    assert!(before(&tail, b"chose No\r\n").is_empty());
    assert_eq!(after(&tail, b"chose No\r\n"), b"No\r\n");

    session.kill_if_alive(Duration::from_secs(5));
}

#[test]
fn pressing_escape_cancels_out_loud() {
    // Sent alone, never appended to other bytes: an escape followed by
    // more input is the start of a sequence and the key parser reads it
    // as one.
    let session = spawn(&["confirm", "Ship it?", "--accessible"]);
    let mut terminal = FakeTerminal::dark();
    let tail = answer_and_drain(&session, &mut terminal, b"\x1b");

    assert_eq!(
        tail,
        b"cancelled\r\n",
        "{:?}",
        String::from_utf8_lossy(&tail)
    );
}

#[test]
fn an_interrupt_and_a_timeout_close_out_loud_too() {
    // Raw mode clears the terminal's signal handling, so this arrives as
    // an ordinary byte and the driver is still running when it writes.
    let session = spawn(&["confirm", "Ship it?", "--accessible"]);
    let mut terminal = FakeTerminal::dark();
    let tail = answer_and_drain(&session, &mut terminal, b"\x03");
    assert_eq!(
        tail,
        b"cancelled\r\n",
        "{:?}",
        String::from_utf8_lossy(&tail)
    );

    let session = spawn(&["confirm", "Ship it?", "--timeout", "300ms", "--accessible"]);
    let mut terminal = FakeTerminal::dark();
    wait_for_in_order(&session, &mut terminal, &[KEYS_ROW], Duration::from_secs(5));
    let tail = drain_for(&session, SILENCE_WINDOW);
    // Two writers say this and they are byte-identical: the transcript
    // row, and the shipped give-up line on stderr after raw mode is off.
    // Counting is what tells them apart.
    assert_eq!(
        count(&tail, b"timed out\r\n"),
        2,
        "the transcript row and the stderr line must both be there: {:?}",
        String::from_utf8_lossy(&tail)
    );

    session.kill_if_alive(Duration::from_secs(5));
}

#[test]
fn the_spoken_answer_and_the_printed_one_are_the_same_string() {
    // Custom labels are where two derivations diverge in practice, and
    // the space in the label also proves the row is not word-split.
    let session = spawn(&[
        "confirm",
        "Ship it?",
        "--affirmative",
        "Ship it",
        "--negative",
        "Wait",
        "--show-output",
        "--accessible",
    ]);
    let mut terminal = FakeTerminal::dark();
    let tail = answer_and_drain(&session, &mut terminal, b"\r");

    assert!(
        contains(&tail, b"chose Ship it\r\n"),
        "{:?}",
        String::from_utf8_lossy(&tail)
    );
    assert_eq!(after(&tail, b"chose Ship it\r\n"), b"Ship it\r\n");

    session.kill_if_alive(Duration::from_secs(5));
}

fn exit_code(session: &PtySession) -> i32 {
    match session.wait_code(Duration::from_secs(5)) {
        Some(code) => code,
        None => panic!(
            "no exit code: exited={}, so it is either still running or died of a signal",
            session.exited()
        ),
    }
}

fn spawn_mode(args: &[&str], transcript: bool) -> PtySession {
    let mut argv = vec!["confirm", "Ship it?"];
    argv.extend_from_slice(args);
    let mut envs = vec![("RAT_APPEARANCE", "dark")];
    if transcript {
        envs.push(("RAT_ACCESSIBLE", "1"));
    }
    PtySession::spawn(&rat_bin(), &argv, &envs).expect("spawn rat confirm under a pty")
}

fn first_output(transcript: bool) -> &'static [u8] {
    if transcript { KEYS_ROW } else { b"\x1b[?25l" }
}

#[test]
fn both_ways_of_ending_a_question_exit_one_and_only_the_row_tells_them_apart() {
    // The strongest entry in the table: same surface, same exit code,
    // opposite events. A reader who answered no and one who abandoned the
    // prompt are distinguished by the row alone — collapse the two rows
    // into one word and they become indistinguishable in both channels at
    // once.
    for transcript in [true, false] {
        let session = spawn_mode(&[], transcript);
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
        assert_eq!(exit_code(&session), 1, "escape, transcript={transcript}");

        let session = spawn_mode(&[], transcript);
        let mut terminal = FakeTerminal::dark();
        wait_for_in_order(
            &session,
            &mut terminal,
            &[first_output(transcript)],
            Duration::from_secs(5),
        );
        if transcript {
            press_and_hear(&session, &mut terminal, b"n", b"chose No\r\n");
        } else {
            session.write_bytes(b"n");
        }
        assert_eq!(exit_code(&session), 1, "negative, transcript={transcript}");
    }
}

#[test]
fn a_custom_negative_label_is_what_the_closing_row_names() {
    // The disagreeing input: a hardcoded label passes both rows above and
    // fails only this one.
    let session = spawn_mode(&["--negative", "Abort"], true);
    let mut terminal = FakeTerminal::dark();
    wait_for_in_order(&session, &mut terminal, &[KEYS_ROW], Duration::from_secs(5));
    press_and_hear(&session, &mut terminal, b"n", b"chose Abort\r\n");
    assert_eq!(exit_code(&session), 1);
}
