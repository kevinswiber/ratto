use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::Context;

use crate::core::measure::seal_rows;

/// Everything the transcript can describe, in the one shape the three
/// surfaces share. Two equal snapshots mean the key changed nothing and
/// the mode says nothing — the comparison is the whole no-op rule, and
/// it lives in one place so no surface has to re-implement it.
// The driver builds one around every key next, and each surface fills
// it in.
#[allow(dead_code)]
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct EchoSnapshot {
    pub cursor: usize,
    pub marked: Vec<bool>,
    pub query: String,
}

/// The row terminator, and the assumption behind it: `run_ui` always
/// enables raw mode, which clears OPOST/ONLCR, so a bare LF drops a row
/// without returning the column. That makes CRLF correct here by
/// construction rather than by choice — and it makes this the one place
/// a future non-raw caller has to look. `rat watch` computes its own
/// terminator for exactly that reason (a `--once` run never takes raw
/// mode); if anything ever reaches this writer without raw mode, it must
/// do the same instead of trusting this constant.
const ECHO_EOL: &str = "\r\n";

/// The transcript's one write, and the only place a row reaches the
/// stream. Nothing here moves the cursor: an appended row is the one
/// thing a screen reader reliably announces, and a row that can be
/// rewritten is a row that can be destroyed mid-word.
///
/// **The stream is the UI stream, never stdout.** The linear stream in
/// `rat watch` has the same shape and writes to `stdout().lock()`;
/// copying that call site here would put transcript rows inside
/// `fruit=$(rat choose …)` and break the capture on the first keystroke.
/// A picker's stdout carries the result and nothing else.
///
/// Each row is neutralized first, so supplied text cannot start a
/// second row or make the terminal do anything the words do not say,
/// and only then sealed as ITS OWN group. The seal is the second line
/// of defense and, with escapes neutralized ahead of it, has nothing
/// left to close — it stays because it is what remains correct if the
/// neutralization is ever narrowed, and because a single-row seal has
/// nothing to replay onto. Rows carrying none of it pass through
/// byte-identical.
// The driver's transcript arm writes every row through this next.
#[allow(dead_code)]
pub fn echo_rows<W: Write>(out: &mut W, rows: &[String]) -> anyhow::Result<()> {
    for row in rows {
        for sealed in seal_rows(vec![flatten_row(row)]) {
            out.write_all(sealed.as_bytes()).context("writing")?;
        }
        out.write_all(ECHO_EOL.as_bytes()).context("writing")?;
    }
    out.flush().context("flushing")
}

/// Every character supplied text may not carry into a transcript row:
/// the C0 controls and DEL. A row must be ONE row and an HONEST row,
/// and a control character is precisely a character the terminal ACTS
/// on rather than displays — it can start a second row (CR, LF, VT,
/// FF), erase or reposition what is already there (ESC, and BS on its
/// own), or make a noise the words never mentioned (BEL). Supplied
/// text has no legitimate use for one. TAB is in the category and that
/// costs nothing: whitespace becomes whitespace.
///
/// The category rather than a list of the ones we thought of: the list
/// grew three times before it was replaced, each time because a person
/// noticed rather than because the rule did.
///
/// NOT `char::is_control()`, which is Unicode's Cc — this set plus C1
/// (U+0080-U+009F). C1 is a different and unruled scope, and reaching
/// for the shorter spelling would widen it as a side effect.
///
/// Deliberately a per-character rule rather than a sequence stripper: a
/// character rule is total, where a parser has to decide what a
/// malformed or truncated sequence is and passes through whatever it
/// does not recognize.
fn is_neutralized(ch: char) -> bool {
    matches!(ch, '\0'..='\x1f' | '\x7f')
}

/// A row's text, made safe to be one honest row: every run of the
/// characters above becomes a single space. Without this, an item
/// carrying a newline — reachable today through `--input-delimiter` —
/// writes a second row that the transcript presents as rat's own words,
/// and an item carrying an escape can erase the rows above it in a mode
/// whose whole promise is that nothing in it erases anything.
///
/// A SPACE, never a deletion. Dropping the character would let
/// `alpha<ESC>beta` arrive as `alphabeta` — one token, a word nobody
/// wrote, which a listener cannot tell from a real one. A space is the
/// only substitution that cannot invent vocabulary. What is left of a
/// sequence survives as ordinary text (`<ESC>[2J` reads as ` [2J`),
/// which is the honest outcome: nothing executes, and nothing the item
/// contained is hidden.
///
/// It does not trim. A trailing break becomes a trailing space, because
/// editing supplied text for a reason unrelated to either claim is not
/// this function's business. Idempotent, so applying it while composing
/// a row and again on the way out costs nothing.
pub fn flatten_row(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_run = false;
    for ch in text.chars() {
        if is_neutralized(ch) {
            if !in_run {
                out.push(' ');
                in_run = true;
            }
        } else {
            out.push(ch);
            in_run = false;
        }
    }
    out
}

/// How many items an opening block lists before it stops naming them and
/// says how many there are. A starting value: the blocks measured in use
/// were a handful of rows, and a list of eight hundred branches must not
/// be read aloud on entry.
// Each surface's entry block is built through the function below next.
#[allow(dead_code)]
pub const ECHO_OPENING_CAP: usize = 20;

/// The block every surface prints once on entry, built in one place so
/// three surfaces cannot drift into three shapes: a header, the items,
/// where the cursor is, and which keys do what.
///
/// A header, position or keys row that was not supplied is dropped — a
/// blank row is a spoken pause with no content. An empty ITEM keeps its
/// row: it is a real item, and dropping it would desynchronize the list
/// from the position row printed under it.
// Each of the three surfaces builds its entry block through this next.
#[allow(dead_code)]
pub fn opening_block(header: &str, items: &[String], position: &str, keys: &str) -> Vec<String> {
    let mut rows = Vec::with_capacity(items.len().min(ECHO_OPENING_CAP) + 4);
    if !header.is_empty() {
        rows.push(flatten_row(header));
    }
    for item in items.iter().take(ECHO_OPENING_CAP) {
        rows.push(flatten_row(item));
    }
    if items.len() > ECHO_OPENING_CAP {
        rows.push(format!("first {ECHO_OPENING_CAP} of {}", items.len()));
    }
    if !position.is_empty() {
        rows.push(flatten_row(position));
    }
    if !keys.is_empty() {
        rows.push(flatten_row(keys));
    }
    rows
}

/// A row waiting for the reader to stop.
struct Pending {
    row: String,
    due: Instant,
}

/// The burst policy: while keys keep arriving, only the newest row
/// survives, and it is handed over once the reader has been quiet for a
/// full interval. A reader typing eleven characters wants to hear where
/// they landed, not eleven intermediate states — and a transcript that
/// narrates every keystroke is one a reader turns off.
///
/// Latest-wins, so the row that is written is always the CURRENT state
/// rather than a stale one. Deliberately pure: it holds no stream and
/// never reads the clock, so every one of its properties can be
/// falsified over synthetic instants without a terminal.
///
/// It decides WHEN, never WHETHER. Whether a key changed anything is the
/// driver's comparison of two snapshots; this type is only ever handed
/// rows that already earned their place.
// The driver's transcript arm holds one of these next.
#[allow(dead_code)]
pub struct Coalescer {
    quiescence: Duration,
    pending: Option<Pending>,
}

// Every method below is reached from the driver's transcript arm next.
#[allow(dead_code)]
impl Coalescer {
    /// The interval is a parameter rather than a constant read from
    /// here: the driver names it at the construction site, where the
    /// value a session runs on is readable, and the tests can drive
    /// small intervals where an off-by-one interval is a failure rather
    /// than a plausible-looking number.
    pub fn new(quiescence: Duration) -> Self {
        Coalescer {
            quiescence,
            pending: None,
        }
    }

    /// Take note of the row the reader's latest key produced. The newest
    /// row replaces any older one and restarts the clock — that restart
    /// is what makes a burst one row instead of a row per interval.
    pub fn note(&mut self, row: String, now: Instant) {
        self.pending = Some(Pending {
            row,
            due: now + self.quiescence,
        });
    }

    /// The row, once the reader has been quiet long enough for it.
    ///
    /// `>=`, not `>`, and that is a liveness rule rather than a rounding
    /// preference: `wait_cap` returns zero once the deadline has passed,
    /// so a strict comparison would leave the loop polling without
    /// blocking, never advancing time, and never writing the row.
    pub fn take_if_due(&mut self, now: Instant) -> Option<String> {
        match &self.pending {
            Some(p) if now >= p.due => self.pending.take().map(|p| p.row),
            _ => None,
        }
    }

    /// How long the loop may sleep without sleeping past a pending row.
    /// With nothing pending it hands back the wait it was given, so a
    /// session with nothing to say polls exactly as it always has. This
    /// is the whole reason a pending row is guaranteed to be written
    /// rather than merely likely to be.
    pub fn wait_cap(&self, now: Instant, base: Duration) -> Duration {
        match &self.pending {
            Some(p) => base.min(p.due.saturating_duration_since(now)),
            None => base,
        }
    }

    /// Drop the pending row unwritten. Called on the way out: a key
    /// pressed inside the interval and followed straight by Enter
    /// contributes no transition row, because the resting state at exit
    /// IS the result and the closing row names it better.
    pub fn discard(&mut self) {
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEYS: &str = "up and down move, enter chooses, escape cancels";

    /// Every escape the in-place engine uses. Deliberately a copy of the
    /// list the pty suites freeze: a unit test in `src/` cannot see
    /// `tests/`, so the duplication cannot be collapsed — only stated.
    /// The two lists are separately frozen and a change to one must not
    /// silently move the other.
    const PAINTING_ESCAPES: [&[u8]; 6] = [
        b"\x1b[?25l",  // hide cursor
        b"\x1b[0J",    // erase below
        b"\x1b[2K",    // erase line
        b"\x1b[?2026", // synchronized output
        b"\x1b[1A",    // cursor up
        b"\x1b[?1049", // alternate screen
    ];

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    /// A short synthetic interval: an off-by-one interval is a visible
    /// failure at this scale and a plausible-looking number at 250 ms.
    const Q: Duration = Duration::from_millis(100);
    /// Stands in for the wait the driver computes for itself.
    const BASE: Duration = Duration::from_millis(250);

    #[test]
    fn a_transcript_row_is_its_words_and_a_carriage_return_newline() {
        let mut out = Vec::new();
        echo_rows(&mut out, &["beta".to_string()]).unwrap();
        assert_eq!(out.as_slice(), b"beta\r\n");

        let mut out = Vec::new();
        echo_rows(&mut out, &["alpha".to_string(), "beta".to_string()]).unwrap();
        assert_eq!(out.as_slice(), b"alpha\r\nbeta\r\n");
    }

    #[test]
    fn supplied_styling_is_neutralized_rather_than_carried_onto_the_stream() {
        let mut out = Vec::new();
        echo_rows(&mut out, &["\x1b[31mred".to_string(), "plain".to_string()]).unwrap();
        assert_eq!(out.as_slice(), b" [31mred\r\nplain\r\n");

        // A row carrying nothing of its own comes out byte-identical, so
        // an ordinary transcript row gains no styling of rat's.
        let mut out = Vec::new();
        echo_rows(&mut out, &["1 of 4".to_string()]).unwrap();
        assert_eq!(out.as_slice(), b"1 of 4\r\n");

        // The total form of the claim: over every fixture this module
        // uses, no row carries a control byte of its own. Only the row
        // terminator does.
        let items: Vec<String> = (1..=847).map(|n| format!("branch {n}")).collect();
        let rows: Vec<String> = [
            "\x1b[31mred".to_string(),
            "safe\x08\x08\x08\x08evil".to_string(),
            "a\x07b".to_string(),
            "a\tb".to_string(),
            "a\x7fb".to_string(),
            "\x1b]0;title\x07".to_string(),
            "\x1b[2J".to_string(),
        ]
        .into_iter()
        .chain(opening_block("Choose:", &items, "1 of 847", KEYS))
        .collect();
        let mut out = Vec::new();
        echo_rows(&mut out, &rows).unwrap();
        for segment in out.split(|&b| b == b'\r' || b == b'\n') {
            assert!(
                segment.iter().all(|&b| b >= 0x20 && b != 0x7f),
                "control byte in {segment:?}"
            );
        }
    }

    #[test]
    fn supplied_text_cannot_forge_a_second_row_or_an_escape_sequence() {
        for (input, want) in [
            ("beta", "beta"),
            ("1 of 4", "1 of 4"),
            ("a\nb", "a b"),
            ("a\rb", "a b"),
            ("a\x0bb", "a b"),
            ("a\x0cb", "a b"),
            ("a\x1bb", "a b"),
            ("safe\x08\x08\x08\x08evil", "safe evil"),
            ("a\x07b", "a b"),
            ("a\tb", "a b"),
            ("a\x7fb", "a b"),
            ("alpha\x1bbeta", "alpha beta"),
            ("a\r\nb", "a b"),
            ("a\n\n\nb", "a b"),
            ("a\r\n\x0b\x0c\x1b\x08b", "a b"),
            ("beta\n", "beta "),
            ("\nbeta", " beta"),
            ("\r", " "),
            ("\x1b[2J", " [2J"),
            ("\x1b]0;title\x07", " ]0;title "),
        ] {
            assert_eq!(flatten_row(input), want, "{input:?}");
        }

        // Every C0 control and DEL, no exceptions and nothing to keep in
        // step by hand. A character added to the predicate is covered here
        // the moment it is added; a character LEFT OUT fails here rather
        // than waiting for someone to notice it.
        for byte in (0x00..=0x1fu8).chain(std::iter::once(0x7f)) {
            let ch = byte as char;
            assert_eq!(flatten_row(&format!("a{ch}b")), "a b", "byte {byte:#04x}");
        }

        // And the complement, so the loop above is a claim about controls
        // rather than about characters: printable text is untouched.
        for byte in 0x20..=0x7eu8 {
            let ch = byte as char;
            let text = format!("a{ch}b");
            assert_eq!(flatten_row(&text), text, "byte {byte:#04x}");
        }

        // An item that IS one of the painting escapes cannot put it back.
        for escape in PAINTING_ESCAPES {
            let item = String::from_utf8_lossy(escape).into_owned();
            let flattened = flatten_row(&item);
            assert!(
                !contains(flattened.as_bytes(), escape),
                "{:?} survived flattening as {flattened:?}",
                String::from_utf8_lossy(escape)
            );
        }

        for s in [
            "beta", "a\r\nb", "a\x0bb", "a\x1bb", "a\x08b", "a\tb", "\x1b[2J", "beta\n", "1 of 4",
        ] {
            assert_eq!(flatten_row(&flatten_row(s)), flatten_row(s), "{s:?}");
        }

        let mut out = Vec::new();
        echo_rows(&mut out, &["alpha\nbeta".to_string()]).unwrap();
        assert_eq!(out.as_slice(), b"alpha beta\r\n");

        // The same claim through the widened predicate: the writer
        // neutralizes by calling the one helper, not by knowing about
        // newlines.
        let mut out = Vec::new();
        echo_rows(&mut out, &["alpha\x0cbeta".to_string()]).unwrap();
        assert_eq!(out.as_slice(), b"alpha beta\r\n");

        // And the honesty claim at the writer: even when the ITEMS are the
        // painting escapes themselves, none of them reaches the stream.
        let forged: Vec<String> = PAINTING_ESCAPES
            .iter()
            .map(|e| format!("item {}", String::from_utf8_lossy(e)))
            .collect();
        let mut out = Vec::new();
        echo_rows(&mut out, &forged).unwrap();
        for escape in PAINTING_ESCAPES {
            assert!(
                !contains(&out, escape),
                "forged {:?} reached the stream",
                String::from_utf8_lossy(escape)
            );
        }
    }

    #[test]
    fn an_empty_row_list_writes_no_bytes() {
        let mut out = Vec::new();
        echo_rows(&mut out, &[]).unwrap();
        assert!(out.is_empty(), "wrote {:?}", String::from_utf8_lossy(&out));
    }

    #[test]
    fn the_transcript_writes_none_of_the_escapes_the_painting_engine_uses() {
        let items: Vec<String> = (1..=847).map(|n| format!("branch {n}")).collect();
        let rows: Vec<String> = [
            "beta".to_string(),
            "\x1b[31mred".to_string(),
            "a\rb".to_string(),
            "a\nb".to_string(),
            "a\x0bb".to_string(),
            "a\x0cb".to_string(),
            "a\x1bb".to_string(),
        ]
        .into_iter()
        .chain(opening_block("Choose:", &items, "1 of 847", KEYS))
        .collect();
        let mut out = Vec::new();
        echo_rows(&mut out, &rows).unwrap();
        for escape in PAINTING_ESCAPES {
            assert!(
                !contains(&out, escape),
                "found {:?} in the transcript",
                String::from_utf8_lossy(escape)
            );
        }
    }

    #[test]
    fn an_opening_block_over_the_cap_lists_the_first_twenty_and_says_how_many_there_are() {
        let items: Vec<String> = (1..=847).map(|n| format!("branch {n}")).collect();
        let rows = opening_block("Choose:", &items, "1 of 847", KEYS);

        assert_eq!(rows[0], "Choose:");
        assert_eq!(rows[1], "branch 1");
        assert_eq!(rows[ECHO_OPENING_CAP], "branch 20");
        // The literal, once: a test that rebuilds the format string the
        // way the code does asserts nothing about the shape.
        assert_eq!(rows[ECHO_OPENING_CAP + 1], "first 20 of 847");
        assert_eq!(rows[ECHO_OPENING_CAP + 2], "1 of 847");
        assert_eq!(rows.last().unwrap(), KEYS);
        assert_eq!(rows.len(), ECHO_OPENING_CAP + 4);
    }

    #[test]
    fn an_opening_block_at_or_under_the_cap_is_the_whole_list_with_no_tail_row() {
        for count in [ECHO_OPENING_CAP - 1, ECHO_OPENING_CAP] {
            let items: Vec<String> = (1..=count).map(|n| format!("branch {n}")).collect();
            let rows = opening_block("Choose:", &items, "1 of 1", KEYS);
            for item in &items {
                assert!(rows.contains(item), "{count}: {item} is missing");
            }
            assert_eq!(rows.len(), items.len() + 3, "{count}");
            assert!(
                !rows.iter().any(|row| row.starts_with("first ")),
                "{count}: a list that fits must not claim to be truncated"
            );
        }

        let over = ECHO_OPENING_CAP + 1;
        let items: Vec<String> = (1..=over).map(|n| format!("branch {n}")).collect();
        let rows = opening_block("Choose:", &items, "1 of 1", KEYS);
        assert_eq!(rows.len(), ECHO_OPENING_CAP + 4);
        assert_eq!(
            rows[ECHO_OPENING_CAP + 1],
            format!("first {ECHO_OPENING_CAP} of {over}")
        );
    }

    #[test]
    fn an_absent_header_is_not_a_blank_row_but_an_empty_item_keeps_its_own() {
        let rows = opening_block("", &["alpha".to_string()], "1 of 1", KEYS);
        assert_eq!(rows, vec!["alpha", "1 of 1", KEYS]);

        // An empty item is an item: dropping it would desynchronize the
        // list from the "N of M" row printed directly under it.
        let rows = opening_block(
            "Choose:",
            &["".to_string(), "beta".to_string()],
            "1 of 2",
            KEYS,
        );
        assert_eq!(rows, vec!["Choose:", "", "beta", "1 of 2", KEYS]);

        let rows = opening_block("Choose:", &["alpha".to_string()], "", KEYS);
        assert_eq!(rows, vec!["Choose:", "alpha", KEYS]);

        let rows = opening_block("Choose:", &["alpha".to_string()], "1 of 1", "");
        assert_eq!(rows, vec!["Choose:", "alpha", "1 of 1"]);
    }

    #[test]
    fn a_snapshot_differs_when_any_one_of_its_three_fields_differs() {
        let base = EchoSnapshot::default();
        assert_eq!(base, EchoSnapshot::default());
        assert_ne!(
            base,
            EchoSnapshot {
                cursor: 1,
                ..base.clone()
            }
        );
        assert_ne!(
            base,
            EchoSnapshot {
                marked: vec![true],
                ..base.clone()
            }
        );
        assert_ne!(
            base,
            EchoSnapshot {
                query: "a".into(),
                ..base.clone()
            }
        );
    }

    #[test]
    fn a_burst_of_keys_leaves_one_row_and_it_is_the_last() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);

        let mut c = Coalescer::new(Q);
        for (n, row) in ["a", "ab", "abc", "abcd", "abcde"].iter().enumerate() {
            c.note((*row).to_string(), at(n as u64 * 20));
        }
        // 100 ms after the FIRST key, and 20 ms after the last: the clock
        // restarted on every note, so nothing is due yet.
        assert_eq!(c.take_if_due(at(100)), None);
        assert_eq!(c.take_if_due(at(180)), Some("abcde".to_string()));
        // Exactly one row: taken once is taken for good.
        assert_eq!(c.take_if_due(at(500)), None);
    }

    #[test]
    fn nothing_is_written_before_the_interval_elapses() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);

        let mut c = Coalescer::new(Q);
        c.note("beta".to_string(), at(0));
        assert_eq!(c.take_if_due(at(0)), None);
        assert_eq!(c.take_if_due(at(99)), None);
        assert_eq!(c.take_if_due(at(100)), Some("beta".to_string()));

        let mut empty = Coalescer::new(Q);
        assert_eq!(empty.take_if_due(at(0)), None);
        assert_eq!(empty.take_if_due(at(10_000)), None);
    }

    #[test]
    fn a_noted_row_is_always_written_within_one_interval_and_one_wait() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);

        let mut c = Coalescer::new(Q);
        let mut now = at(0);
        c.note("beta".to_string(), now);

        let mut written = None;
        // Bounded on purpose: an implementation that never comes due would
        // otherwise HANG here, because wait_cap goes to zero once the
        // deadline passes and nothing advances the clock.
        for _ in 0..100 {
            let wait = c.wait_cap(now, BASE);
            now += wait;
            if let Some(row) = c.take_if_due(now) {
                written = Some((row, now));
                break;
            }
        }
        let (row, when) = written.expect("a noted row was never written");
        assert_eq!(row, "beta");
        assert!(when <= at(0) + Q + BASE, "flushed late");

        // An interval LARGER than the base, so the cap is taken from the
        // base on the early iterations and from the remaining time on the
        // last — the arm that proves the cap composes over several waits
        // rather than only over one.
        let long = Duration::from_millis(600);
        let mut c = Coalescer::new(long);
        let mut now = at(0);
        c.note("delta".to_string(), now);
        let mut written = None;
        for _ in 0..100 {
            let wait = c.wait_cap(now, BASE);
            now += wait;
            if let Some(row) = c.take_if_due(now) {
                written = Some((row, now));
                break;
            }
        }
        let (row, when) = written.expect("a noted row was never written");
        assert_eq!(row, "delta");
        assert_eq!(when, at(600));
        assert!(when <= at(0) + long + BASE, "flushed late");
    }

    #[test]
    fn the_wait_cap_never_outruns_the_time_remaining() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);

        let mut c = Coalescer::new(Q);
        c.note("beta".to_string(), at(0));
        for (now, base, want) in [
            (at(0), BASE, Duration::from_millis(100)),
            (at(60), BASE, Duration::from_millis(40)),
            (at(0), Duration::from_millis(40), Duration::from_millis(40)),
            (at(100), BASE, Duration::ZERO),
            (at(400), BASE, Duration::ZERO),
        ] {
            assert_eq!(c.wait_cap(now, base), want, "{base:?}");
        }

        // With nothing pending the driver's own wait comes back
        // untouched: a session with nothing to say polls exactly as it
        // always has.
        let idle = Coalescer::new(Q);
        for base in [
            Duration::ZERO,
            Duration::from_millis(40),
            BASE,
            Duration::from_secs(60),
        ] {
            assert_eq!(idle.wait_cap(at(0), base), base, "{base:?}");
            assert_eq!(idle.wait_cap(at(10_000), base), base, "{base:?}");
        }
    }

    #[test]
    fn a_discarded_row_never_arrives_and_the_next_one_still_does() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);

        // A key pressed inside the interval and followed straight by
        // enter contributes no transition row: the resting state at exit
        // IS the result, and the closing row names it better.
        let mut c = Coalescer::new(Q);
        c.note("beta".to_string(), at(0));
        c.discard();
        assert_eq!(c.take_if_due(at(10_000)), None);
        assert_eq!(c.wait_cap(at(0), BASE), BASE);

        // Discarding empties the pending row; it does not poison the type.
        c.note("delta".to_string(), at(200));
        assert_eq!(c.take_if_due(at(300)), Some("delta".to_string()));
    }
}
