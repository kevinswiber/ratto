//! The `{{name}}` template layer: the reference grammar, the `substitute`
//! free function, and `Template` — a string recorded as written alongside
//! the names it references, never expanded at parse time.
//!
//! Knows nothing about KDL, panes, or variables: every consumer supplies
//! the map for its own moment.

/// The map a substitution runs against — INV-2's `Map`. A plain
/// name → text map by design: `substitute` knows nothing about tiers,
/// KDL, or panes, which is exactly what lets a later key-action or
/// prompt layer hand it `variables ∪ that binding's prompt answers`.
///
/// `BTreeMap`, not `HashMap`: a teaching error lists the declared set,
/// and a set that lists in a different order on every run is a
/// diffable output nobody can diff.
pub type Bindings = std::collections::BTreeMap<String, String>;

/// A reference to a name the map does not hold. Carries what a placed
/// error needs and nothing more: the name, and where in THIS string it
/// was written. Mapping that offset into a document's line and column
/// is the caller's job — the split is what lets other consumers
/// reuse `substitute` behind a completely different error surface.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MissingVariable {
    pub name: String,
    pub offset: usize,
}

impl std::fmt::Display for MissingVariable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown variable `{}`", self.name)
    }
}

impl std::error::Error for MissingVariable {}

/// A variable name: `[A-Za-z_][A-Za-z0-9_-]*` (INV-1). The ONE
/// spelling — a reference matches it, and the `variables` block refuses
/// a declared name that does not, so a variable that could never be
/// written as `{{name}}` is refused where it is declared.
pub fn is_reference_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The reference beginning at `text[at..]`, if one does: its name and
/// the byte just past its closing `}}`.
///
/// Nothing else in a string is a reference — no whitespace inside, no
/// expressions, no nesting — so a `{{` that fails here is text the
/// author wrote and is left alone.
///
/// `pub` rather than private: the `script` first-bytes rule (INV-7 /
/// NEW-1) needs exactly this question — "does this body BEGIN with a
/// reference?" — which `refs` cannot answer because it records names
/// without positions. The alternative is that check respelling
/// `{{`/`}}` itself and getting `{{ name }}` (inner whitespace) or
/// `{{{{a}}}}` wrong; this function already handles both correctly.
///
/// **Precondition, same family as `substitute`'s:** this reads bytes,
/// so a caller must have established the string's FLAVOR first. A raw
/// string's bytes are indistinguishable from a normal one's (INV-1),
/// and asking this about a raw body would find a reference that INV-1
/// says is not there. Recorded sites go through `Template`, whose
/// `refs` already encodes the answer; reach for this only when the
/// question is positional, and guard it on `interpolates`.
pub fn reference_at(text: &str, at: usize) -> Option<(&str, usize)> {
    // UTF-8-safe by construction: `strip_prefix` and `find` return
    // char-boundary offsets, and `is_reference_name` is ASCII-only, so
    // no slice here can split a multi-byte character.
    let rest = text.get(at..)?.strip_prefix("{{")?;
    let end = rest.find("}}")?;
    let name = &rest[..end];
    is_reference_name(name).then_some((name, at + 2 + end + 2))
}

/// Every reference in `text`, each with the byte range it occupies.
/// The ONE scanner extraction and substitution share, so a hole they
/// disagree about cannot exist.
fn scan(text: &str) -> Vec<(std::ops::Range<usize>, &str)> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(next) = text[at..].find("{{") {
        let start = at + next;
        match reference_at(text, start) {
            Some((name, end)) => {
                out.push((start..end, name));
                at = end;
            }
            // Not a reference: the WHOLE brace run is literal text
            // (INV-1), and it must be consumed atomically. Stepping
            // two bytes would land inside `{{{{plan}}}}` and match the
            // `{{plan}}` a reader wrote precisely to be literal —
            // producing `{{0028}}`, a silent substitution, which is
            // the one failure mode INV-6 forbids outright. The run is
            // at least two bytes, so `at` always advances.
            None => at = start + text[start..].bytes().take_while(|&b| b == b'{').count(),
        }
    }
    out
}

/// Expand every `{{name}}` in `text` against `bindings` (INV-2).
///
/// A free function over `&str` and a map: not a method on the walk,
/// not on `DashboardFile`, not on the spawn site. What each consumer
/// supplies is the map for ITS moment — variables at load, variables
/// at pane spawn, variables ∪ prompt answers at a key-action spawn.
///
/// Prefer [`Template::expand`] wherever a recorded site is being
/// expanded: this function re-scans bytes, and a raw string's bytes
/// look exactly like a normal string's (INV-1).
pub fn substitute(text: &str, bindings: &Bindings) -> Result<String, MissingVariable> {
    let holes = scan(text);
    if holes.is_empty() {
        return Ok(text.to_string());
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (range, name) in holes {
        let value = bindings.get(name).ok_or_else(|| MissingVariable {
            name: name.to_string(),
            offset: range.start,
        })?;
        out.push_str(&text[at..range.start]);
        out.push_str(value);
        at = range.end;
    }
    out.push_str(&text[at..]);
    Ok(out)
}

/// A string as written, plus the names it references. Extraction
/// never expands (INV-2): the walk records, the use site substitutes.
/// The fields are PRIVATE, deliberately: `pub text` would make
/// `substitute(&t.text, …)` exactly as reachable as `t.expand(…)`, and
/// only one of those is correct once a raw string's bytes are
/// indistinguishable from a normal one's (INV-1). Re-deriving from the
/// bytes when the answer is already in the record is a bug class that
/// occurred three times during design alone; a visible `as_str()` is
/// the same protection the absent `Deref` provides, applied to the
/// other half.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Template {
    /// The author's bytes, holes and all.
    text: String,
    /// Every name referenced, in first-appearance order, deduplicated
    /// — a name written twice is one dependency, not two. ALWAYS
    /// empty for a raw string (INV-1), which is what makes raw-ness a
    /// property of the RECORD rather than a re-scan at every use.
    refs: Vec<String>,
    /// The KDL flavor this came from. Beside `refs` for exactly one
    /// reason: `command` is word-split at load (INV-7), and each word
    /// must be re-recorded under the ORIGINAL value's flavor —
    /// without this a raw `#"{{a}} b"#` would acquire a reference the
    /// moment it was split.
    interpolates: bool,
}

impl Template {
    /// The bytes as written. Deliberately NOT a `Deref<Target = str>`:
    /// a template that silently reads as a `&str` is a template
    /// someone will hand to `substitute` after raw strings made a raw
    /// value's bytes indistinguishable from a normal one's.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Every name this string references — empty for a literal and for
    /// EVERY raw string, which is what the whole raw rule rests on.
    pub fn refs(&self) -> &[String] {
        &self.refs
    }

    /// The recorded flavor: `true` for a normal string, `false` for a
    /// raw one (and for `literal`-constructed values generally).
    // The site rule's positional guard is this accessor's production
    // caller; until it lands only tests read it.
    #[allow(dead_code)]
    pub fn interpolates(&self) -> bool {
        self.interpolates
    }

    /// Record a RAW KDL string: literal end to end, `{{` not even
    /// recognized (INV-1). Records zero references, which is what
    /// makes raw-ness a property of the RECORD rather than a re-scan
    /// at every use. The flavor check that picks between this and
    /// `extract` at the KDL extraction sites is raw-string support's
    /// job; nothing chooses `literal` until it lands.
    pub fn literal(text: &str) -> Template {
        Template {
            text: text.to_string(),
            refs: Vec::new(),
            interpolates: false,
        }
    }

    /// Record a NORMAL KDL string: `{{name}}` interpolates.
    pub fn extract(text: &str) -> Template {
        let mut refs: Vec<String> = Vec::new();
        for (_, name) in scan(text) {
            if !refs.iter().any(|seen| seen == name) {
                refs.push(name.to_string());
            }
        }
        Template {
            text: text.to_string(),
            refs,
            interpolates: true,
        }
    }

    /// The ONE expansion entry point for a recorded site. A template
    /// with no references — a literal, or ANY raw string — returns its
    /// bytes untouched without consulting the map at all. That is what
    /// keeps expansion TOTAL for the overwhelming majority of boards
    /// and what makes INV-1's raw rule hold at every site without a
    /// second check anywhere.
    pub fn expand(&self, bindings: &Bindings) -> Result<String, MissingVariable> {
        if self.refs.is_empty() {
            return Ok(self.text.clone());
        }
        substitute(&self.text, bindings)
    }

    /// Re-record each word of a word-split value under THIS value's
    /// flavor — INV-7's argv-boundary rule seen from the extraction
    /// side. The split happens on TEMPLATE text at load, so each word
    /// is a template of its own, and an expansion lands inside the
    /// word that held it.
    pub fn reslice(&self, words: Vec<String>) -> Vec<Template> {
        words
            .into_iter()
            .map(|word| {
                if self.interpolates {
                    Template::extract(&word)
                } else {
                    Template {
                        text: word,
                        refs: Vec::new(),
                        interpolates: false,
                    }
                }
            })
            .collect()
    }
}

/// The NORMAL-string constructor, for the hundreds of existing
/// `PaneDecl` literals in the test suite. Production code at the three
/// extraction sites picks the constructor from the KDL entry's own
/// flavor and never reaches for this.
impl From<&str> for Template {
    fn from(text: &str) -> Template {
        Template::extract(text)
    }
}

impl From<String> for Template {
    fn from(text: String) -> Template {
        Template::extract(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reference_is_two_braces_an_identifier_and_two_braces() {
        let map = Bindings::from([
            ("plan".to_string(), "0028".to_string()),
            ("a-b_c".to_string(), "ok".to_string()),
            ("_x".to_string(), "under".to_string()),
        ]);
        assert_eq!(substitute("plan {{plan}}", &map).unwrap(), "plan 0028");
        assert_eq!(substitute("{{plan}}", &map).unwrap(), "0028");
        assert_eq!(substitute("{{plan}}{{plan}}", &map).unwrap(), "00280028");
        // The identifier grammar: leading letter or `_`, then letters,
        // digits, `_`, `-`.
        assert_eq!(substitute("{{a-b_c}}/{{_x}}", &map).unwrap(), "ok/under");
        // No inner whitespace, no expressions, no nesting.
        for literal in [
            "{{ plan }}",
            "{{plan }}",
            "{{ plan}}",
            "{{plan.name}}",
            "{{plan|upper}}",
            "{{9lives}}",
            "{{{{plan}}}}",
            "{{{plan}}}",
        ] {
            assert_eq!(substitute(literal, &map).unwrap(), literal, "{literal:?}");
            assert!(Template::extract(literal).refs.is_empty(), "{literal:?}");
        }
    }

    #[test]
    fn a_brace_run_that_is_not_a_reference_stays_literal() {
        // INV-6: a jq or awk body is never touched, and the only failure
        // mode is a NAMED error, never a silent substitution.
        let map = Bindings::new();
        for body in [
            "{{",
            "}}",
            "{{}}",
            ".items[] | {{name: .n}}",
            "awk '{{print $1}}'",
            "{ } {{",
        ] {
            assert_eq!(substitute(body, &map).unwrap(), body, "{body:?}");
            assert!(Template::extract(body).refs.is_empty(), "{body:?}");
        }
        // A brace run that is not a reference is consumed ATOMICALLY: the
        // scanner must not re-enter `{{{{plan}}}}` and match the
        // `{{plan}}` inside it. This is the assertion that fails if the
        // failure path advances by two bytes instead of past the run —
        // and the failure is a SILENT substitution, not an error.
        let with_plan = Bindings::from([("plan".to_string(), "0028".to_string())]);
        assert_eq!(
            substitute("{{{{plan}}}} and {{plan}}", &with_plan).unwrap(),
            "{{{{plan}}}} and 0028",
            "the run is skipped whole, and scanning resumes after it"
        );
    }

    #[test]
    fn an_unknown_name_names_itself_and_where_it_sits() {
        let map = Bindings::from([("plan".to_string(), "0028".to_string())]);
        let err = substitute("cd {{plan}} && cat {{nope}}", &map).expect_err("nope is unknown");
        assert_eq!(err.name, "nope");
        // The offset is a byte index into THIS string; placing it in a
        // document is the caller's job.
        assert_eq!(err.offset, "cd {{plan}} && cat ".len());
        // The FIRST unknown name is reported: it is the one the author
        // can act on.
        let err = substitute("{{a}} {{b}}", &Bindings::new()).expect_err("both unknown");
        assert_eq!(err.name, "a");
        assert_eq!(err.offset, 0);
    }

    #[test]
    fn extraction_records_the_names_and_never_expands() {
        let t = Template::extract("cd {{plan}} && git -C {{plan}} log {{rev}}");
        // The author's bytes survive whole — INV-2: the walk records, the
        // use site expands.
        assert_eq!(t.text, "cd {{plan}} && git -C {{plan}} log {{rev}}");
        // First-appearance order, deduplicated: a name referenced twice is
        // one dependency, not two.
        assert_eq!(t.refs, vec!["plan".to_string(), "rev".to_string()]);
        assert!(t.interpolates);
    }

    #[test]
    fn a_template_free_string_records_no_references() {
        let t = Template::extract("git log --oneline -3");
        assert!(t.refs.is_empty());
        // The path spawn-time expansion short-circuits on, and the
        // reason a board with
        // no variables carries zero new failure modes.
        assert_eq!(t.expand(&Bindings::new()).unwrap(), "git log --oneline -3");
    }

    #[test]
    fn expansion_consults_the_recorded_references_not_the_bytes() {
        // The pin that makes raw strings work (INV-1) and the
        // reason `expand` exists beside `substitute`: a template whose
        // record says it references nothing is returned VERBATIM without
        // the map being consulted at all. A caller that re-scanned the
        // text instead would expand a raw string.
        let t = Template::literal("plan {{plan}}");
        let map = Bindings::from([("plan".to_string(), "0028".to_string())]);
        assert_eq!(t.expand(&map).unwrap(), "plan {{plan}}");
    }

    #[test]
    fn a_split_value_keeps_its_words_under_the_original_flavor() {
        // INV-7's argv boundary rule: `command` is word-split at LOAD, on
        // TEMPLATE text, and each word is re-recorded — under the whole
        // value's flavor, so a raw string's words cannot acquire a
        // reference the split invented.
        let t = Template::extract("git -C {{plan}} log");
        let words = t.reslice(vec![
            "git".into(),
            "-C".into(),
            "{{plan}}".into(),
            "log".into(),
        ]);
        assert_eq!(
            words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(),
            vec!["git", "-C", "{{plan}}", "log"]
        );
        assert_eq!(words[2].refs, vec!["plan".to_string()]);
        assert!(words[0].refs.is_empty());

        let raw = Template::literal("git -C {{plan}} log");
        let words = raw.reslice(vec!["git".into(), "{{plan}}".into()]);
        assert!(words[1].refs.is_empty(), "a raw value's words stay literal");
    }

    #[test]
    fn a_name_is_valid_by_the_same_grammar_everywhere() {
        for ok in ["plan", "_x", "a-b", "A9", "z_9-z"] {
            assert!(is_reference_name(ok), "{ok}");
        }
        for bad in ["", "9lives", "-lead", "a.b", "a b", "a|b", "ünicode"] {
            assert!(!is_reference_name(bad), "{bad}");
        }
    }
}
