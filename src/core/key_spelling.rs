//! The spelling a board writes for a key, and the key it names.
//!
//! | Written | Means |
//! |---|---|
//! | one ASCII printable — `"r"`, `"7"`, `"?"`, and `"["`/`"]"`/`"O"` | [`Key::Char`] |
//! | a named key — `"Enter"`, `"PageDown"` (thirteen) | that variant |
//! | `"Alt-"` and one ASCII printable **except `[`, `]`, `O`** | [`Key::Alt`] |
//!
//! The grammar is unambiguous by length: every named key is at least
//! three characters, a bare character is exactly one, and the chord
//! form carries a `-`.
//!
//! **Nothing else is spellable, because nothing else arrives on every
//! wire.** Two readers deliver keys — `from_crossterm` (`ui/key.rs:36`)
//! on Windows, and `TapScanner` (`term/tap.rs`) for an ordinary unix
//! board — and this table is their INTERSECTION. The tap is the
//! narrower of the two: `decode_key` (`tap.rs:191-200`) passes only
//! `0x03`, `0x09`, `\r`/`\n`, `0x20` and `0x21..=0x7e`, and
//! `decode_csi` (`:252-267`) has no `Delete`. So control chords,
//! `Backspace`, `Delete` and every non-ASCII character are refused
//! here even though the `Key` enum can hold them and Windows can
//! deliver them: a binding that fires on one platform and silently
//! does nothing on the other is worse than one that never loads.
//!
//! **And three Alt chords go the same way for a different reason.**
//! `ESC [`, `ESC ]` and `ESC O` are the CSI, OSC and SS3 introducers,
//! so `TapScanner::feed` (`tap.rs:106`) routes them into sequence
//! reassembly before `decode_meta` is consulted — `Alt-[`, `Alt-]` and
//! `Alt-O` never arrive. (`Alt-o`, lowercase, is fine; so are the bare
//! characters `[`, `]` and `O`, which are single bytes like any
//! other.)
//!
//! Widening the wire is not this module's business. When the tap
//! learns a new key, exactly one table below changes.

use std::borrow::Cow;

use anyhow::anyhow;

use crate::ui::key::Key;

/// Every key with a written name, and its ONE spelling. Read by the
/// parser, by [`spelling_of`], and by the refusal's teaching tail — so
/// a row added here is spellable, renderable and advertised at once.
///
/// **Thirteen rows, not the enum's fifteen.** `Backspace` and `Delete`
/// are absent because the unix tap delivers neither — `decode_key`
/// (`tap.rs:191-200`) drops `0x08` and `0x7f`, and `decode_csi`
/// (`:252-267`) has no `\x1b[3~` arm. They are also the only two named
/// keys no built-in claims, which is exactly why leaving them out
/// matters: they are the pair a board author would reach for first,
/// and they would work on Windows and nowhere else.
const NAMED_KEYS: &[(&str, Key)] = &[
    ("Enter", Key::Enter),
    ("Esc", Key::Esc),
    ("Tab", Key::Tab),
    ("BackTab", Key::BackTab),
    ("Space", Key::Space),
    ("Up", Key::Up),
    ("Down", Key::Down),
    ("Left", Key::Left),
    ("Right", Key::Right),
    ("Home", Key::Home),
    ("End", Key::End),
    ("PageUp", Key::PageUp),
    ("PageDown", Key::PageDown),
];

/// A character a board may spell, bare or under `Alt-`. ASCII
/// printables, matching both `decode_key`'s `0x21..=0x7e` arm
/// (`tap.rs:197`) and `decode_meta`'s (`:207`). `0x20` is excluded
/// here and named `Space` instead, which is what both wires do.
fn is_spellable_char(c: char) -> bool {
    matches!(c, '\u{21}'..='\u{7e}')
}

/// A character a board may spell **under `Alt-`**. The bare set minus
/// three: `ESC [`, `ESC ]` and `ESC O` are the CSI, OSC and SS3
/// introducers, and `TapScanner::feed` (`tap.rs:106`) routes them into
/// sequence reassembly before `decode_meta` is ever consulted. Those
/// three chords cannot arrive however they are written.
///
/// `O` is uppercase only — `Alt-o` is an ordinary chord.
fn is_alt_spellable_char(c: char) -> bool {
    is_spellable_char(c) && !matches!(c, '[' | ']' | 'O')
}

/// A written spelling as the key it names.
///
/// The error carries **no breadcrumb** — this module does not know
/// where the spelling was written. The caller adds it with
/// `.with_context(|| at)`, which renders as `key "F5": …` under
/// `{:#}`, the same shape `resolve_source` gets from
/// `parse_trigger(text).with_context(|| at(id))`
/// (`dashboard_file.rs:378`).
pub fn parse_key(spelling: &str) -> anyhow::Result<Key> {
    if let Some((_, key)) = NAMED_KEYS.iter().find(|(name, _)| *name == spelling) {
        return Ok(*key);
    }
    if let Some(rest) = spelling.strip_prefix("Ctrl-") {
        if rest.starts_with("Alt-") {
            return Err(refuse(spelling, BOTH_MODIFIERS));
        }
        return Err(refuse(spelling, NO_CONTROL_CHORDS));
    }
    if let Some(rest) = spelling.strip_prefix("Alt-") {
        if rest.starts_with("Ctrl-") {
            return Err(refuse(spelling, BOTH_MODIFIERS));
        }
        return match one_char(rest) {
            // `decode_meta` (`tap.rs:205-210`) and `from_crossterm`
            // (`ui/key.rs:45-48`) both map ALT onto a printable only,
            // so `Alt-Enter` and `Alt-<space>` name nothing.
            Some(c) if is_alt_spellable_char(c) => Ok(Key::Alt(c)),
            Some(c) if is_spellable_char(c) => Err(refuse(spelling, ALT_INTRODUCER)),
            Some(c) if !c.is_ascii() => Err(refuse(spelling, NOT_ASCII)),
            _ => Err(refuse(spelling, NO_SUCH_KEY)),
        };
    }
    match one_char(spelling) {
        Some(c) if is_spellable_char(c) => Ok(Key::Char(c)),
        Some(' ') => Err(refuse(spelling, "a space is written `Space`")),
        Some('\t') => Err(refuse(spelling, "a tab is written `Tab`")),
        Some('\r' | '\n') => Err(refuse(spelling, "a newline is written `Enter`")),
        Some(c) if !c.is_ascii() => Err(refuse(spelling, NOT_ASCII)),
        Some(_) => Err(refuse(spelling, NO_SUCH_KEY)),
        None if spelling.is_empty() => Err(refuse(spelling, EMPTY)),
        None => Err(refuse(spelling, why_not(spelling))),
    }
}

/// The one spelling a key renders as — the inverse of [`parse_key`],
/// built from the same two tables and pinned by a round-trip test.
///
/// **For keys with no declaration to echo.** A surface naming a
/// binding the author wrote uses that binding's own recorded spelling
/// instead, so the displayed bytes are the file's bytes. This is for
/// the other case: naming a key rat itself owns, or enumerating the
/// whole key space.
///
/// A total match with no `_` arm: a new [`Key`] variant fails to
/// compile here, which is the only place in the crate that can notice
/// one has appeared. Do not "simplify" it into a wildcard — that would
/// turn the compile-time notice into a runtime `expect`.
pub fn spelling_of(key: Key) -> String {
    match key {
        Key::Char(c) => c.to_string(),
        Key::Alt(c) => format!("Alt-{c}"),
        // Rendered for completeness — a refusal or a diagnostic may
        // still have to NAME one of these — but never parseable back,
        // which is why they are not in the tables.
        Key::CtrlA => "Ctrl-a".to_string(),
        Key::CtrlC => "Ctrl-c".to_string(),
        Key::CtrlE => "Ctrl-e".to_string(),
        Key::CtrlU => "Ctrl-u".to_string(),
        Key::CtrlW => "Ctrl-w".to_string(),
        Key::Backspace => "Backspace".to_string(),
        Key::Delete => "Delete".to_string(),
        Key::Up
        | Key::Down
        | Key::Left
        | Key::Right
        | Key::Home
        | Key::End
        | Key::PageUp
        | Key::PageDown
        | Key::Enter
        | Key::Esc
        | Key::Tab
        | Key::BackTab
        | Key::Space => NAMED_KEYS
            .iter()
            .find(|(_, k)| *k == key)
            .map(|(name, _)| (*name).to_string())
            .expect("every named key is in the table"),
    }
}

const NO_SUCH_KEY: &str = "no key is spelled that way";
const EMPTY: &str = "a binding needs a key to name";
const BOTH_MODIFIERS: &str = "Alt and Ctrl together arrive as neither chord";
const NO_FUNCTION_KEYS: &str = "rat never sees a function key";
/// The tap decodes one control byte, `0x03` (`tap.rs:193`), and every
/// board already spends it on abort. There is no chord left to bind.
const NO_CONTROL_CHORDS: &str = "rat does not read control chords as bindings";
/// `decode_key` (`tap.rs:197`) passes `0x21..=0x7e` and drops the rest,
/// so a non-ASCII binding would work on Windows and nowhere else.
const NOT_ASCII: &str = "rat reads its keys as ASCII";
/// `Backspace` and `Delete` reach crossterm but not the tap.
const NOT_EVERY_PLATFORM: &str = "rat does not read that key on every platform";
/// `ESC [`, `ESC ]` and `ESC O` open a sequence instead of a chord
/// (`tap.rs:106`), so those three Alt spellings never arrive.
const ALT_INTRODUCER: &str =
    "that byte opens an escape sequence, so rat never sees it as an Alt chord";

/// Every refusal is the same sentence: what was written, why it cannot
/// be a key, and the whole grammar. The tail is generated from the two
/// tables, so a spelling that becomes legal is advertised the moment
/// its row exists.
fn refuse(spelling: &str, reason: impl std::fmt::Display) -> anyhow::Error {
    anyhow!(
        "`{spelling}` is not a key a binding can name — {reason}. {}",
        spellable()
    )
}

fn spellable() -> String {
    format!(
        // The Alt exclusion is IN the sentence, not merely in the
        // parser: a teaching tail that offers a spelling the parser
        // refuses sends the author straight into a second error.
        "A binding names one ASCII character (`r`, `7`, `?`), a named key ({}), \
         or `Alt-` and one ASCII character other than `[`, `]` or `O`",
        NAMED_KEYS
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(", "),
    )
}

/// The reason a multi-character spelling names nothing. A near miss on
/// a named key gets the exact spelling — the grammar is case-sensitive
/// because bare characters are (`S` snapshots, `s` is free), so
/// folding case here would make one half of the grammar disagree with
/// the other.
fn why_not(spelling: &str) -> Cow<'static, str> {
    // A key the vocabulary HAS but a wire does not deliver gets its
    // own sentence: "no key is spelled that way" would be a lie, and
    // the author needs to know it is a platform ceiling rather than a
    // typo.
    if spelling.eq_ignore_ascii_case("Backspace") || spelling.eq_ignore_ascii_case("Delete") {
        return NOT_EVERY_PLATFORM.into();
    }
    if let Some((name, _)) = NAMED_KEYS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(spelling))
    {
        return format!("write `{name}`").into();
    }
    if spelling.len() > 1
        && spelling.starts_with('F')
        && spelling[1..].chars().all(|c| c.is_ascii_digit())
    {
        return NO_FUNCTION_KEYS.into();
    }
    NO_SUCH_KEY.into()
}

/// One character, or `None` for zero or more than one.
fn one_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// Every key a board can spell — the finite input space the conflict
/// matrix enumerates: the thirteen named keys, every ASCII printable
/// as a bare character, and the same range under `Alt-` **minus `[`,
/// `]` and `O`**, which open an escape sequence instead.
///
/// It IS the spellable universe rather than an ASCII slice of it,
/// because nothing outside ASCII is spellable; the name is kept
/// because the consumers cite it.
///
/// `#[cfg(test)]` and `pub(crate)`: it exists to keep tests honest,
/// not to be a runtime capability. Unit tests compile the whole crate
/// under `cfg(test)`, so a test module in another file reaches it by
/// its full path.
#[cfg(test)]
pub(crate) fn ascii_spellable() -> Vec<Key> {
    let printable = || (0x21u8..=0x7e).map(char::from);
    NAMED_KEYS
        .iter()
        .map(|(_, key)| *key)
        .chain(printable().map(Key::Char))
        .chain(
            printable()
                .filter(|c| is_alt_spellable_char(*c))
                .map(Key::Alt),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spelled(text: &str) -> Key {
        parse_key(text).unwrap_or_else(|err| panic!("`{text}` should name a key: {err:#}"))
    }

    fn refused(text: &str) -> String {
        format!(
            "{:#}",
            parse_key(text).expect_err("the spelling is refused")
        )
    }

    #[test]
    fn a_bare_character_is_its_own_spelling() {
        assert_eq!(spelled("r"), Key::Char('r'));
        assert_eq!(spelled("7"), Key::Char('7'));
        assert_eq!(spelled("?"), Key::Char('?'));
        assert_eq!(spelled("<"), Key::Char('<'));
        // Case is load-bearing: `S` snapshots and `s` is free, so a
        // spelling that folded case would silently bind the wrong one.
        assert_eq!(spelled("R"), Key::Char('R'));
        assert_ne!(spelled("r"), spelled("R"));
        // Non-ASCII is REFUSED, and not for tidiness: the unix tap
        // decodes bytes, and everything above 0x7e falls to `None`
        // (`tap.rs:197`). A board binding `é` would load, advertise
        // itself, and never fire outside Windows.
        assert!(refused("é").contains("rat reads its keys as ASCII"));
    }

    #[test]
    fn every_named_key_has_exactly_one_spelling_and_it_round_trips() {
        // The whole table, both directions. A key that parses but does not
        // render is a key the `?` section cannot advertise (4.1) and the
        // conflict error cannot name (1.3).
        for (spelling, key) in [
            ("Enter", Key::Enter),
            ("Esc", Key::Esc),
            ("Tab", Key::Tab),
            ("BackTab", Key::BackTab),
            ("Space", Key::Space),
            ("Up", Key::Up),
            ("Down", Key::Down),
            ("Left", Key::Left),
            ("Right", Key::Right),
            ("Home", Key::Home),
            ("End", Key::End),
            ("PageUp", Key::PageUp),
            ("PageDown", Key::PageDown),
        ] {
            assert_eq!(spelled(spelling), key, "{spelling}");
            assert_eq!(spelling_of(key), spelling, "{key:?}");
        }
    }

    #[test]
    fn the_two_keys_only_windows_delivers_are_not_spellable() {
        // `Backspace` and `Delete` exist in the vocabulary and arrive
        // through crossterm, but the unix tap has no arm for either:
        // `decode_key` (`:191-200`) drops 0x08 and 0x7f, and `decode_csi`
        // (`:252-267`) has no `\x1b[3~`. They are also, by coincidence,
        // the only two named keys no built-in claims — so refusing them
        // is what keeps a board from binding the one pair that looks free
        // and works on one platform.
        for spelling in ["Backspace", "Delete"] {
            let err = refused(spelling);
            assert!(
                err.contains("rat does not read that key on every platform"),
                "{spelling} → {err}"
            );
        }
    }

    #[test]
    fn alt_spells_its_modifier_and_round_trips() {
        for (spelling, key) in [
            ("Alt-h", Key::Alt('h')),
            ("Alt-1", Key::Alt('1')),
            ("Alt-?", Key::Alt('?')),
        ] {
            assert_eq!(spelled(spelling), key, "{spelling}");
            assert_eq!(spelling_of(key), spelling, "{key:?}");
        }
        // `Char` renders as itself, which is what makes `spelling_of` the
        // one renderer every surface can use.
        assert_eq!(spelling_of(Key::Char('r')), "r");
        // `Alt-` and a space is not a thing on the wire either
        // (`tap.rs:205-210` excludes 0x20 deliberately).
        assert!(refused("Alt- ").contains("is not a key a binding can name"));
    }

    #[test]
    fn the_three_alt_chords_that_open_a_sequence_are_refused() {
        // `ESC [`, `ESC ]` and `ESC O` are the CSI, OSC and SS3
        // introducers, so `TapScanner::feed` (`tap.rs:106`) routes them
        // into reassembly and `decode_meta` never sees them.
        for spelling in ["Alt-[", "Alt-]", "Alt-O"] {
            let err = refused(spelling);
            assert!(
                err.contains("that byte opens an escape sequence"),
                "{spelling} → {err}"
            );
        }
        // Case matters: only uppercase `O` introduces SS3.
        assert_eq!(spelled("Alt-o"), Key::Alt('o'));
        // The bare characters are untouched — one byte is just a byte.
        for spelling in ["[", "]", "O"] {
            assert!(matches!(spelled(spelling), Key::Char(_)), "{spelling}");
        }
    }

    #[test]
    fn the_refused_alt_chords_really_do_not_arrive() {
        // The falsification arm. Refusing three spellings on a claim about
        // the wire is worth nothing unless the wire is asked: feed each
        // sequence and assert the scanner produces NO key — it is holding
        // bytes for a sequence that has not finished.
        use crate::term::tap::{TapEvent, TapScanner};
        for bytes in [&b"\x1b["[..], &b"\x1b]"[..], &b"\x1bO"[..]] {
            assert_eq!(TapScanner::new().feed(bytes), vec![], "{bytes:?}");
        }
        // And the control: `ESC o` is not an introducer, so it resolves
        // immediately as the chord.
        assert_eq!(
            TapScanner::new().feed(b"\x1bo"),
            vec![TapEvent::Key(Key::Alt('o'))]
        );
    }

    #[test]
    fn every_spelling_the_parser_accepts_arrives_on_both_wires() {
        // The property that makes "spellable" mean something, and the
        // reason it is TWO assertions rather than one: a spelling that
        // only crossterm delivers is a binding that loads, advertises
        // itself in `?`, and never fires on an ordinary unix board.
        // Nothing reports that — which is why it has to be impossible to
        // declare rather than merely documented.
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        use crate::term::tap::{TapEvent, TapScanner};
        use crate::ui::key::from_crossterm;

        // (key, crossterm code+modifiers, the tap's bytes)
        for (key, code, modifiers, bytes) in [
            (Key::Enter, KeyCode::Enter, KeyModifiers::NONE, &b"\r"[..]),
            (Key::Tab, KeyCode::Tab, KeyModifiers::NONE, &b"\t"[..]),
            (
                Key::BackTab,
                KeyCode::BackTab,
                KeyModifiers::NONE,
                &b"\x1b[Z"[..],
            ),
            (
                Key::Space,
                KeyCode::Char(' '),
                KeyModifiers::NONE,
                &b" "[..],
            ),
            (Key::Up, KeyCode::Up, KeyModifiers::NONE, &b"\x1b[A"[..]),
            (Key::Down, KeyCode::Down, KeyModifiers::NONE, &b"\x1b[B"[..]),
            (Key::Left, KeyCode::Left, KeyModifiers::NONE, &b"\x1b[D"[..]),
            (
                Key::Right,
                KeyCode::Right,
                KeyModifiers::NONE,
                &b"\x1b[C"[..],
            ),
            (Key::Home, KeyCode::Home, KeyModifiers::NONE, &b"\x1b[H"[..]),
            (Key::End, KeyCode::End, KeyModifiers::NONE, &b"\x1b[F"[..]),
            (
                Key::PageUp,
                KeyCode::PageUp,
                KeyModifiers::NONE,
                &b"\x1b[5~"[..],
            ),
            (
                Key::PageDown,
                KeyCode::PageDown,
                KeyModifiers::NONE,
                &b"\x1b[6~"[..],
            ),
        ] {
            assert_eq!(spelled(&spelling_of(key)), key, "{key:?} parses back");
            assert_eq!(
                from_crossterm(KeyEvent::new(code, modifiers)),
                Some(key),
                "{key:?} arrives on the crossterm wire"
            );
            assert_eq!(
                TapScanner::new().feed(bytes),
                vec![TapEvent::Key(key)],
                "{key:?} arrives on the tap wire"
            );
        }

        // EVERY accepted character, both forms, both wires — not a
        // sample. Ninety-four bare and ninety-one Alt chords is a cheap
        // loop, and a sampled matrix is exactly how `Alt-[` survived the
        // first draft of this task.
        for byte in 0x21u8..=0x7e {
            let c = char::from(byte);

            assert_eq!(spelled(&c.to_string()), Key::Char(c), "bare {c:?} parses");
            assert_eq!(
                from_crossterm(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
                Some(Key::Char(c)),
                "bare {c:?} on crossterm"
            );
            assert_eq!(
                TapScanner::new().feed(&[byte]),
                vec![TapEvent::Key(Key::Char(c))],
                "bare {c:?} on the tap"
            );

            let alt = format!("Alt-{c}");
            if matches!(c, '[' | ']' | 'O') {
                assert!(parse_key(&alt).is_err(), "{alt} is refused");
                continue;
            }
            assert_eq!(spelled(&alt), Key::Alt(c), "{alt} parses");
            assert_eq!(
                from_crossterm(KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)),
                Some(Key::Alt(c)),
                "{alt} on crossterm"
            );
            assert_eq!(
                TapScanner::new().feed(&[0x1b, byte]),
                vec![TapEvent::Key(Key::Alt(c))],
                "{alt} on the tap"
            );
        }

        // `Esc` is the one spelling with no single feed: a lone ESC
        // resolves only after `ESC_HOLD` of silence (`tap.rs:168-178`), so
        // it is driven the way `a_lone_escape_resolves_after_the_hold`
        // (`tap.rs:768`) drives it.
        assert_eq!(spelled("Esc"), Key::Esc);
        assert_eq!(
            from_crossterm(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Some(Key::Esc)
        );
        let mut scanner = TapScanner::new();
        assert_eq!(scanner.feed(b"\x1b"), vec![]);
        scanner.idle(std::time::Duration::from_millis(20));
        assert_eq!(
            scanner.idle(std::time::Duration::from_millis(40)),
            vec![TapEvent::Key(Key::Esc)]
        );
    }

    #[test]
    fn a_function_key_is_refused_and_says_so() {
        let err = refused("F5");
        assert!(
            err.contains("`F5` is not a key a binding can name"),
            "{err}"
        );
        assert!(err.contains("rat never sees a function key"), "{err}");
        // `F` alone is a plain character — and a claimed one (1.3), which
        // is a different refusal entirely.
        assert_eq!(spelled("F"), Key::Char('F'));
    }

    #[test]
    fn no_control_chord_is_spellable() {
        // The vocabulary has five, and the unix tap delivers exactly one
        // of them (`Ctrl-c`, `tap.rs:193`) — which every board already
        // uses to abort. There is no chord a binding could take, so the
        // whole form is out rather than narrowed to a single claimed key.
        for spelling in [
            "Ctrl-a", "Ctrl-c", "Ctrl-e", "Ctrl-u", "Ctrl-w", "Ctrl-x", "Ctrl-",
        ] {
            let err = refused(spelling);
            assert!(
                err.contains("is not a key a binding can name"),
                "{spelling} → {err}"
            );
            assert!(
                err.contains("rat does not read control chords as bindings"),
                "{spelling} → {err}"
            );
        }
    }

    #[test]
    fn alt_and_ctrl_together_is_refused_in_either_order() {
        for spelling in ["Alt-Ctrl-c", "Ctrl-Alt-c"] {
            let err = refused(spelling);
            assert!(
                err.contains("Alt and Ctrl together arrive as neither chord"),
                "{spelling} → {err}"
            );
        }
    }

    #[test]
    fn whitespace_and_control_characters_are_not_bare_characters() {
        // A KDL string can hold them, and they are invisible in the file.
        // Both have a named spelling, so the refusal hands it over.
        assert!(refused(" ").contains("a space is written `Space`"));
        assert!(refused("\t").contains("a tab is written `Tab`"));
        // Anything else non-printing keeps the general refusal.
        assert!(refused("\u{7}").contains("is not a key a binding can name"));
    }

    #[test]
    fn an_empty_spelling_names_no_key() {
        assert!(refused("").contains("a binding needs a key to name"));
    }

    #[test]
    fn a_near_miss_on_a_named_key_teaches_the_exact_spelling() {
        // The spellings are exact, and the grammar is unambiguous by
        // length — every named key is at least three characters and a bare
        // character is exactly one — so a case-folded miss can always be
        // answered with the one spelling the author meant.
        assert!(refused("enter").contains("write `Enter`"));
        assert!(refused("ESC").contains("write `Esc`"));
        assert!(refused("pageup").contains("write `PageUp`"));
    }

    #[test]
    fn every_refusal_says_what_is_spellable() {
        // The whole grammar in every refusal, from ONE derivation — the
        // ceiling is invisible from the KDL side, so a refusal that only
        // declines leaves the author guessing.
        for spelling in [
            "F5",
            "Ctrl-x",
            "Alt-Ctrl-c",
            " ",
            "",
            "enter",
            "nope",
            "é",
            "Delete",
            "Alt-[",
        ] {
            let err = refused(spelling);
            assert!(
                err.contains("A binding names one ASCII character"),
                "{spelling} → {err}"
            );
            // The tail states the Alt exclusion too. Without it the
            // sentence offers `Alt-` and any character, which is the one
            // place the grammar can still over-promise after the parser
            // stopped accepting it.
            assert!(
                err.contains("`Alt-` and one ASCII character other than `[`, `]` or `O`"),
                "{spelling} → {err}"
            );
            assert!(
                err.contains(
                    "Enter, Esc, Tab, BackTab, Space, Up, Down, Left, Right, \
                              Home, End, PageUp, PageDown"
                ),
                "{spelling} → {err}"
            );
            // The list is the tables', so a spelling that is NOT offered
            // never appears in the sentence offering spellings.
            assert!(!err.contains("Ctrl-a"), "{spelling} → {err}");
            assert!(!err.contains("Backspace"), "{spelling} → {err}");
        }
    }

    #[test]
    fn the_enumerated_space_is_the_space_the_parser_accepts() {
        // `ascii_spellable()` is 1.3's matrix input, and a matrix over a
        // list that has drifted from the parser proves nothing. Every
        // member round-trips; nothing outside is silently included.
        for key in ascii_spellable() {
            assert_eq!(spelled(&spelling_of(key)), key, "{key:?}");
        }
        assert!(ascii_spellable().contains(&Key::Char('r')));
        assert!(ascii_spellable().contains(&Key::Alt('9')));
        assert!(ascii_spellable().contains(&Key::PageDown));
    }
}
