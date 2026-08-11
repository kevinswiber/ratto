#![cfg(unix)]
mod common;

use std::time::Duration;

use common::pty::{FakeTerminal, PtySession, drain_for, wait_for_in_order};

fn rat_bin() -> String {
    assert_cmd::cargo::cargo_bin("rat").display().to_string()
}

/// The block's last row, in each of its two forms. Named once, because
/// every test in this suite anchors on it and because it must stay in
/// step with what the surface says.
///
/// Anchoring on it answers two hazards at once: it is the block's LAST
/// row, so hearing it has consumed the block, and it sits past the
/// terminal's echo of this test's own item bytes — so no test after the
/// opening block has to think about the echo at all.
const KEYS_ONE: &[u8] =
    b"type to filter, up and down move, enter chooses, escape cancels, control o says where you are\r\n";
#[allow(dead_code)]
const KEYS_MULTI: &[u8] =
    b"type to filter, up and down move, tab selects, enter confirms, escape cancels, control o says where you are, control t says what you selected\r\n";

/// One quiescence interval plus one poll slice, plus margin for a loaded
/// CI box. There is no library target to import those constants from, so
/// the relation lives here and must move when they do. A shorter drain
/// does not measure silence — it measures a row still in flight.
const SILENCE_WINDOW: Duration = Duration::from_millis(1000);

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.len() <= haystack.len() && haystack.windows(needle.len()).any(|w| w == needle)
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

/// The rows strictly between two anchor rows of one capture. The
/// terminal's echo of this test's own item bytes sits ahead of the first
/// anchor, so judging the block means judging this slice — never the
/// whole stream.
fn rows_between(seen: &[u8], open: &[u8], close: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(before(after(seen, open), close))
        .split("\r\n")
        .filter(|r| !r.is_empty())
        .map(str::to_string)
        .collect()
}

/// Press one key and hear the row it causes, before anything else is
/// pressed. Race-free by construction: the next row cannot already be in
/// flight inside this wait's final chunk, because nothing has been
/// pressed yet to cause it.
#[allow(dead_code)]
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
#[allow(dead_code)]
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

/// Spawn a filter and feed it `items` the way a pipeline would, then end
/// the list.
///
/// The end-of-input marker is what makes this work: the candidates are
/// read to EOF before the picker takes raw mode, and in the terminal's
/// default line mode `\x04` on an empty line is that EOF. It does not
/// close anything — the keystrokes a test sends next still arrive.
///
/// A pinned appearance is not decoration here. Left to detect, the
/// startup probe would query the terminal, the fake terminal would
/// answer by writing into the session, and that reply would arrive while
/// the candidates are still being read — that is, it would become an
/// item.
///
/// `--no-fuzzy` throughout: fuzzy matching is subsequence matching, so
/// `ap` also matches `banana split`. These tests are about what the
/// picker says, not about how it ranks.
///
/// **If this mechanic ever proves unreliable** — the child hangs reading
/// its candidates, or the item bytes are truncated at the terminal's
/// canonical line limit — the fallback is to dup a pipe onto the child's
/// input after it takes the terminal, and write the items into that.
/// Keys still arrive, because the key path opens the controlling
/// terminal directly once the input is not a terminal. It is not the
/// first choice because it changes the spawn signature three other
/// suites already call.
fn spawn_filter(items: &[&str], extra: &[&str]) -> (PtySession, FakeTerminal) {
    let mut args = vec!["filter", "--accessible", "--no-fuzzy"];
    args.extend_from_slice(extra);
    let session = PtySession::spawn(&rat_bin(), &args, &[("RAT_APPEARANCE", "dark")])
        .expect("spawn rat filter under a pty");
    let mut fed = items.join("\n");
    fed.push_str("\n\x04");
    session.write_bytes(fed.as_bytes());
    (session, FakeTerminal::dark())
}

#[test]
fn the_opening_block_follows_a_piped_item_list() {
    let (session, mut terminal) =
        spawn_filter(&["apple", "apricot", "banana"], &["--header", "Fruit"]);
    wait_for_in_order(
        &session,
        &mut terminal,
        &[
            // Only rat can have written this: the header came from argv,
            // never from the master. Opening the chain on an item name
            // would match the terminal's echo of this test's own bytes.
            b"Fruit\r\n",
            b"apple\r\n",
            b"apricot\r\n",
            b"banana\r\n",
            b"1 of 3\r\n",
            KEYS_ONE,
        ],
        Duration::from_secs(5),
    );

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(2));
}

#[test]
fn a_key_after_the_end_of_input_marker_still_reaches_the_picker() {
    let (session, mut terminal) =
        spawn_filter(&["apple", "apricot", "banana"], &["--header", "Fruit"]);
    wait_for_in_order(&session, &mut terminal, &[KEYS_ONE], Duration::from_secs(5));

    // Two keys, and what they print — not what they say. The printed
    // line is the picker's shipped output and owes nothing to the
    // transcript's wording, which is asserted where that wording lives.
    // Moving and then taking is what makes this a statement about the
    // channel: the second candidate can only be printed if BOTH keys
    // arrived, in order, after the end-of-input marker.
    //
    // This is the one test that fails when the item channel and the key
    // channel stop being the same channel — the condition that sends
    // this suite to the fallback described on the spawn helper.
    session.write_bytes(b"\x1b[B\r");
    let tail = drain_for(&session, SILENCE_WINDOW);
    assert!(
        contains(&tail, b"apricot\r\n"),
        "both keys must reach the picker after the marker: {:?}",
        String::from_utf8_lossy(&tail)
    );
    assert!(!session.kill_if_alive(Duration::from_secs(5)));
}

#[test]
fn a_headerless_run_still_says_where_the_cursor_is() {
    let (session, mut terminal) = spawn_filter(&["apple", "apricot", "banana"], &[]);
    let seen = wait_for_in_order(
        &session,
        &mut terminal,
        &[b"1 of 3\r\n", KEYS_ONE],
        Duration::from_secs(5),
    );
    // Nothing sits between the position row and the keys row — in
    // particular no blank row where a header would have been.
    assert_eq!(
        rows_between(&seen, b"1 of 3\r\n", KEYS_ONE),
        Vec::<String>::new(),
        "{:?}",
        String::from_utf8_lossy(&seen)
    );

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(2));
}

#[test]
fn an_entry_query_that_matches_nothing_says_so_instead_of_a_position() {
    // A seeded query narrows the matches before the block is built, so
    // this is also the test that proves the block lists MATCHES rather
    // than items: a block built from the input would name all three
    // fruits under a query that matches none of them.
    let (session, mut terminal) = spawn_filter(
        &["apple", "apricot", "banana"],
        &["--header", "Fruit", "--value", "zz"],
    );
    let seen = wait_for_in_order(
        &session,
        &mut terminal,
        &[b"Fruit\r\n", KEYS_ONE],
        Duration::from_secs(5),
    );
    assert_eq!(
        rows_between(&seen, b"Fruit\r\n", KEYS_ONE),
        ["no matches"],
        "{:?}",
        String::from_utf8_lossy(&seen)
    );

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(2));
}

#[test]
fn a_long_list_is_capped_at_the_opening() {
    let items: Vec<String> = (1..=25).map(|n| format!("item{n:02}")).collect();
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    let (session, mut terminal) = spawn_filter(&refs, &["--header", "Many"]);
    let seen = wait_for_in_order(
        &session,
        &mut terminal,
        &[b"Many\r\n", KEYS_ONE],
        Duration::from_secs(5),
    );
    let rows = rows_between(&seen, b"Many\r\n", KEYS_ONE);

    // The shape, not a hand-written literal: the cap's value and the
    // tail row's wording belong to the shared builder, and a literal
    // here would be a second place to change them.
    let listed: Vec<&String> = rows
        .iter()
        .filter(|r| r.starts_with("item") && r.len() == 6)
        .collect();
    assert_eq!(listed.len(), 20, "{rows:?}");
    assert_eq!(listed[0], "item01");
    assert_eq!(listed[19], "item20");
    assert!(
        !rows.iter().any(|r| r.contains("item21")),
        "the twenty-first item must not be spoken: {rows:?}"
    );
    assert!(rows.iter().any(|r| r == "first 20 of 25"), "{rows:?}");
    assert!(rows.iter().any(|r| r == "1 of 25"), "{rows:?}");

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(2));
}

#[test]
fn an_item_that_carries_a_line_break_is_still_one_row() {
    // Candidates that carry line breaks, reached through a different
    // input delimiter: the line discipline hands the reader its lines,
    // the run rejoins and splits on the comma, so the three candidates
    // are literally `one\ntwo`, `three\x0cfour` and `five`.
    //
    // The form feed is the arm no other surface can reach — a candidate
    // carrying one is what a log pipeline or a pasted record produces —
    // and a build that flattens only the newline passes the first half
    // and fails this one.
    let session = PtySession::spawn(
        &rat_bin(),
        &[
            "filter",
            "--accessible",
            "--no-fuzzy",
            "--input-delimiter",
            ",",
            "--header",
            "Fruit",
        ],
        &[("RAT_APPEARANCE", "dark")],
    )
    .expect("spawn rat filter under a pty");
    let mut terminal = FakeTerminal::dark();
    session.write_bytes(b"one\ntwo,three\x0cfour,five\n\x04");

    let seen = wait_for_in_order(
        &session,
        &mut terminal,
        &[b"Fruit\r\n", KEYS_ONE],
        Duration::from_secs(5),
    );
    // Judging the slice below the header is mandatory: `two` and `four`
    // are also in the terminal's echo of this test's own bytes, so an
    // assertion over the whole stream would fail on the echo rather than
    // on the picker — and would keep failing after a fix.
    let rows = rows_between(&seen, b"Fruit\r\n", KEYS_ONE);
    assert_eq!(rows.len(), 4, "three candidates and a position: {rows:?}");
    // `five` is a real third candidate and its own row is correct; the
    // orphans are the halves a break would have split off.
    for orphan in ["two", "four"] {
        assert!(
            !rows.iter().any(|r| r == orphan),
            "a candidate must not forge a row: {rows:?}"
        );
    }
    assert!(rows.iter().any(|r| r == "five"), "{rows:?}");
    assert!(contains(seen.as_slice(), b"1 of 3\r\n"), "{rows:?}");

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(2));
}

/// Anchor past the opening block, then send a burst and listen long
/// enough that anything the burst policy was holding has certainly
/// landed — and anything it should have suppressed has had every chance
/// to leak. The rows returned are only what the picker wrote after the
/// block, so the terminal's echo of this test's own item bytes is
/// outside the window being judged.
fn burst_and_rows(session: &PtySession, terminal: &mut FakeTerminal, keys: &[u8]) -> Vec<String> {
    wait_for_in_order(session, terminal, &[KEYS_ONE], Duration::from_secs(5));
    session.write_bytes(keys);
    let tail = drain_for(session, SILENCE_WINDOW);
    String::from_utf8_lossy(&tail)
        .split("\r\n")
        .filter(|r| !r.is_empty())
        .map(str::to_string)
        .collect()
}

fn three_items() -> [&'static str; 3] {
    ["alpha bravo one", "alpha bravo two", "charlie delta"]
}

#[test]
fn a_typed_burst_speaks_only_the_query_it_settles_on() {
    let (session, mut terminal) = spawn_filter(&three_items(), &["--header", "Items"]);

    // Eleven characters in ONE write, so the whole burst is in the
    // terminal's input queue before the picker reads the first of them —
    // which is what a fast typist looks like to the driver.
    //
    // This is the one test in this suite licensed to send several keys
    // at once: coalescing is its SUBJECT rather than its hazard, and the
    // assertion is over the settled result rather than an ordered chain.
    // Paced into eleven separate presses it would stop proving anything.
    let rows = burst_and_rows(&session, &mut terminal, b"alpha bravo");
    assert_eq!(rows, ["alpha bravo, 2 matches, alpha bravo one"]);

    // Stated separately so the intent survives someone loosening the
    // equality above: without the debounce there is one row per
    // character, and this names WHICH state leaked.
    for n in 1.."alpha bravo".len() {
        let leaked = format!("{}, ", &"alpha bravo"[..n]);
        assert!(
            !rows.iter().any(|r| r.starts_with(&leaked)),
            "an intermediate query was spoken: {leaked:?} in {rows:?}"
        );
    }
    // And nothing re-listed the candidates: the block is written once.
    for absent in ["alpha bravo two", "charlie delta"] {
        assert!(
            !rows.iter().any(|r| r.contains(absent)),
            "{absent:?} in {rows:?}"
        );
    }

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(2));
}

#[test]
fn a_query_that_matches_nothing_says_so() {
    // Not silent: "no matches" is a state the reader needs, and it is
    // what the painted mode showed as an empty list.
    let (session, mut terminal) = spawn_filter(&three_items(), &["--header", "Items"]);
    let rows = burst_and_rows(&session, &mut terminal, b"zzz");
    assert_eq!(rows, ["zzz, no matches"]);

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(2));
}

#[test]
fn a_query_with_one_match_reads_in_the_singular() {
    // The row is read aloud, and "1 matches" is a stumble in the one
    // place this mode exists to be listened to.
    let (session, mut terminal) = spawn_filter(&three_items(), &["--header", "Items"]);
    let rows = burst_and_rows(&session, &mut terminal, b"charlie");
    assert_eq!(rows, ["charlie, 1 match, charlie delta"]);

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(2));
}

#[test]
fn editing_the_query_backwards_settles_the_same_way() {
    // Deleting reaches the query through the same path, changes the same
    // field, and must produce the same SHAPE of row — one, on settle. A
    // transcript written per key rather than per state would say
    // "deleted" or repeat the whole query four times.
    let (session, mut terminal) = spawn_filter(&three_items(), &["--header", "Items"]);
    let rows = burst_and_rows(&session, &mut terminal, b"alpha bravo");
    assert_eq!(rows, ["alpha bravo, 2 matches, alpha bravo one"]);

    session.write_bytes(b"\x7f\x7f\x7f\x7f");
    let tail = drain_for(&session, SILENCE_WINDOW);
    let rows: Vec<String> = String::from_utf8_lossy(&tail)
        .split("\r\n")
        .filter(|r| !r.is_empty())
        .map(str::to_string)
        .collect();
    assert_eq!(rows, ["alpha b, 2 matches, alpha bravo one"]);

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(2));
}

#[test]
fn submitting_inside_the_window_drops_the_row_it_was_holding() {
    // The query and the submit in one write, inside one quiescence
    // interval: the state at exit is the result, and the row the burst
    // policy was holding is discarded rather than spoken.
    //
    // A later change that "fixes" the apparent loss by flushing the
    // pending row on exit fails right here.
    let (session, mut terminal) = spawn_filter(&three_items(), &["--header", "Items"]);
    wait_for_in_order(&session, &mut terminal, &[KEYS_ONE], Duration::from_secs(5));
    session.write_bytes(b"alpha\r");
    let tail = drain_for(&session, SILENCE_WINDOW);
    assert!(
        contains(&tail, b"chose alpha bravo one\r\n"),
        "the closing row must arrive: {:?}",
        String::from_utf8_lossy(&tail)
    );
    assert!(
        before(&tail, b"chose alpha bravo one\r\n").is_empty(),
        "the held query row must be dropped, not flushed: {:?}",
        String::from_utf8_lossy(before(&tail, b"chose alpha bravo one\r\n"))
    );

    session.kill_if_alive(Duration::from_secs(2));
}

fn fruit() -> [&'static str; 3] {
    ["apple", "apricot", "banana"]
}

/// Answer, then hand back the whole tail. The drain is long enough that
/// a wrongly flushed row would have arrived.
fn answer_and_drain(session: &PtySession, keys: &[u8]) -> Vec<u8> {
    session.write_bytes(keys);
    drain_for(session, SILENCE_WINDOW)
}

#[test]
fn a_cursor_move_names_the_match_it_lands_on() {
    let (session, mut terminal) = spawn_filter(&fruit(), &["--header", "Fruit"]);
    let mut seen = wait_for_in_order(&session, &mut terminal, &[KEYS_ONE], Duration::from_secs(5));
    seen.extend(press_and_hear(
        &session,
        &mut terminal,
        b"\x1b[B",
        b"apricot\r\n",
    ));
    seen.extend(press_and_hear(
        &session,
        &mut terminal,
        b"\x1b[B",
        b"banana\r\n",
    ));
    seen.extend(press_and_hear(
        &session,
        &mut terminal,
        b"\x1b[A",
        b"apricot\r\n",
    ));
    assert_eq!(
        common::pty::first_unmatched_in_order(
            &seen,
            &[KEYS_ONE, b"apricot\r\n", b"banana\r\n", b"apricot\r\n"],
        ),
        None,
        "{:?}",
        String::from_utf8_lossy(&seen)
    );

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(2));
}

#[test]
fn a_cursor_move_that_cannot_happen_says_nothing() {
    let (session, mut terminal) = spawn_filter(&fruit(), &["--header", "Fruit"]);
    wait_for_in_order(&session, &mut terminal, &[KEYS_ONE], Duration::from_secs(5));

    // Up at the top of the list.
    session.write_bytes(b"\x1b[A");
    assert_silent_then_alive(&session, &mut terminal, (b"\x1b[B", b"apricot\r\n"));

    // Down at the bottom: settle there first, hearing each row, so the
    // burst policy is empty when the drain starts.
    press_and_hear(&session, &mut terminal, b"\x1b[B", b"banana\r\n");
    session.write_bytes(b"\x1b[B");
    assert_silent_then_alive(&session, &mut terminal, (b"\x1b[A", b"apricot\r\n"));

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(2));
}

#[test]
fn a_toggle_names_what_it_marked_and_what_it_unmarked() {
    let (session, mut terminal) = spawn_filter(&fruit(), &["--header", "Fruit", "--no-limit"]);
    wait_for_in_order(
        &session,
        &mut terminal,
        &[KEYS_MULTI],
        Duration::from_secs(5),
    );
    press_and_hear(&session, &mut terminal, b"\x1b[B", b"apricot\r\n");
    press_and_hear(&session, &mut terminal, b"\t", b"selected apricot\r\n");
    press_and_hear(&session, &mut terminal, b"\t", b"deselected apricot\r\n");

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(2));
}

#[test]
fn marking_a_second_item_in_single_select_names_the_one_it_marked() {
    // Single-select clears the old mark as it sets the new one, and the
    // row names what the reader DID. A rule that reported the first
    // differing index would say `deselected apple` here.
    let (session, mut terminal) = spawn_filter(&fruit(), &["--header", "Fruit"]);
    wait_for_in_order(&session, &mut terminal, &[KEYS_ONE], Duration::from_secs(5));
    press_and_hear(&session, &mut terminal, b"\t", b"selected apple\r\n");
    press_and_hear(&session, &mut terminal, b"\x1b[B", b"apricot\r\n");
    press_and_hear(&session, &mut terminal, b"\t", b"selected apricot\r\n");

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(2));
}

#[test]
fn a_toggle_the_limit_refuses_says_nothing() {
    let (session, mut terminal) = spawn_filter(&fruit(), &["--header", "Fruit", "--limit", "2"]);
    wait_for_in_order(
        &session,
        &mut terminal,
        &[KEYS_MULTI],
        Duration::from_secs(5),
    );
    press_and_hear(&session, &mut terminal, b"\t", b"selected apple\r\n");
    press_and_hear(&session, &mut terminal, b"\x1b[B", b"apricot\r\n");
    press_and_hear(&session, &mut terminal, b"\t", b"selected apricot\r\n");
    press_and_hear(&session, &mut terminal, b"\x1b[B", b"banana\r\n");

    session.write_bytes(b"\t");
    assert_silent_then_alive(&session, &mut terminal, (b"\x1b[A", b"apricot\r\n"));

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(2));
}

#[test]
fn the_closing_row_names_what_stdout_prints() {
    let (session, mut terminal) = spawn_filter(&fruit(), &["--header", "Fruit"]);
    wait_for_in_order(&session, &mut terminal, &[KEYS_ONE], Duration::from_secs(5));
    press_and_hear(&session, &mut terminal, b"\x1b[B", b"apricot\r\n");

    // Two writers on one device, told apart by content: the closing row
    // is written inside the driver in raw mode, and the printed line
    // after it. Their agreement is the shared accessor made visible.
    let tail = answer_and_drain(&session, b"\r");
    assert!(
        contains(&tail, b"chose apricot\r\n"),
        "{:?}",
        String::from_utf8_lossy(&tail)
    );
    assert!(before(&tail, b"chose apricot\r\n").is_empty());
    assert_eq!(after(&tail, b"chose apricot\r\n"), b"apricot\r\n");

    session.kill_if_alive(Duration::from_secs(2));
}

#[test]
fn a_multi_select_closing_names_them_in_the_order_stdout_prints() {
    let (session, mut terminal) = spawn_filter(&fruit(), &["--header", "Fruit", "--no-limit"]);
    wait_for_in_order(
        &session,
        &mut terminal,
        &[KEYS_MULTI],
        Duration::from_secs(5),
    );
    // Marked OUT of list order on purpose: what stdout prints is list
    // order, so that is what the row must say.
    press_and_hear(&session, &mut terminal, b"\x1b[B", b"apricot\r\n");
    press_and_hear(&session, &mut terminal, b"\x1b[B", b"banana\r\n");
    press_and_hear(&session, &mut terminal, b"\t", b"selected banana\r\n");
    press_and_hear(&session, &mut terminal, b"\x1b[A", b"apricot\r\n");
    press_and_hear(&session, &mut terminal, b"\t", b"selected apricot\r\n");

    let tail = answer_and_drain(&session, b"\r");
    assert!(
        contains(&tail, b"chose apricot, banana\r\n"),
        "list order, not selection order: {:?}",
        String::from_utf8_lossy(&tail)
    );
    assert_eq!(
        after(&tail, b"chose apricot, banana\r\n"),
        b"apricot\r\nbanana\r\n"
    );

    session.kill_if_alive(Duration::from_secs(2));
}

#[test]
fn a_non_strict_run_speaks_the_query_it_prints() {
    // The input where two independent derivations disagree: nothing
    // matched, but a non-strict run prints the query, so a row derived
    // from the selection alone would say `chose nothing` while stdout
    // said `zz`. Read together with its strict twin below.
    let (session, mut terminal) = spawn_filter(&fruit(), &["--header", "Fruit", "--no-strict"]);
    wait_for_in_order(&session, &mut terminal, &[KEYS_ONE], Duration::from_secs(5));
    press_and_hear(&session, &mut terminal, b"zz", b"zz, no matches\r\n");

    let tail = answer_and_drain(&session, b"\r");
    assert!(
        contains(&tail, b"chose zz\r\n"),
        "the transcript must name what stdout prints: {:?}",
        String::from_utf8_lossy(&tail)
    );
    assert!(before(&tail, b"chose zz\r\n").is_empty());
    assert_eq!(after(&tail, b"chose zz\r\n"), b"zz\r\n");

    session.kill_if_alive(Duration::from_secs(2));
}

#[test]
fn a_strict_run_says_it_chose_nothing_and_prints_nothing() {
    // The same keystrokes one flag apart. A closing row derived from the
    // selection alone passes THIS and fails its non-strict twin.
    let (session, mut terminal) = spawn_filter(&fruit(), &["--header", "Fruit"]);
    wait_for_in_order(&session, &mut terminal, &[KEYS_ONE], Duration::from_secs(5));
    press_and_hear(&session, &mut terminal, b"zz", b"zz, no matches\r\n");

    let tail = answer_and_drain(&session, b"\r");
    assert_eq!(
        tail,
        b"chose nothing\r\n",
        "this run prints not one byte: {:?}",
        String::from_utf8_lossy(&tail)
    );

    session.kill_if_alive(Duration::from_secs(2));
}

#[test]
fn pressing_escape_cancels_and_prints_nothing() {
    let (session, mut terminal) = spawn_filter(&fruit(), &["--header", "Fruit"]);
    wait_for_in_order(&session, &mut terminal, &[KEYS_ONE], Duration::from_secs(5));

    // The empty tail after the row is also the proof that an aborted run
    // prints nothing: the run propagates the driver's error and never
    // reaches its print.
    let tail = answer_and_drain(&session, b"\x1b");
    assert_eq!(
        tail,
        b"cancelled\r\n",
        "{:?}",
        String::from_utf8_lossy(&tail)
    );
}

#[test]
fn both_questions_are_answered_mid_query_and_the_query_is_untouched() {
    let (session, mut terminal) = spawn_filter(&fruit(), &["--header", "Fruit"]);
    wait_for_in_order(&session, &mut terminal, &[KEYS_ONE], Duration::from_secs(5));

    // A row shape no echo of this test's own bytes can produce.
    press_and_hear(&session, &mut terminal, b"ap", b"ap, 2 matches, apple\r\n");

    // Only the driver can write these: the reducers have no vocabulary
    // at all, so this is the arm that fails against a missing intercept.
    // Two writes rather than one — each chord answers immediately and
    // independently, so nothing here needs a pending row.
    press_and_hear(&session, &mut terminal, b"\x0f", b"apple 1 of 2\r\n");
    press_and_hear(&session, &mut terminal, b"\x14", b"nothing selected\r\n");

    // And the query took the `r` and nothing else: neither chord reached
    // the editor.
    press_and_hear(&session, &mut terminal, b"r", b"apr, 1 match, apricot\r\n");

    session.write_bytes(b"\x1b");
    session.kill_if_alive(Duration::from_secs(5));
}
