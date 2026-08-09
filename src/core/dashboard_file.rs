//! The parsed dashboard declaration and the ONE path that validates it.
//!
//! The constructor (`dashboard_kdl`) produces a [`DashboardFile`] and
//! nothing else; every token in a dashboard file is parsed exactly
//! once, here, so the grammar walk and the rules can never drift
//! apart.
//!
//! ## Interval resolution
//!
//! | pane `interval` | defaults `interval` | triggers | effective |
//! |---|---|---|---|
//! | a token | any | any | the pane's token |
//! | `"never"` | any | any | none — triggers only |
//! | absent | a token | any | the default's token |
//! | absent | absent | some | none — triggers only |
//! | absent | absent | none | 2s |
//!
//! `"never"` is spelled out HERE and nowhere else: teaching it to
//! `core::duration::parse_interval` would leak into `rat watch -n`.

use anyhow::{Context, anyhow, bail};

use crate::core::box_model::{BorderPreset, Sides, parse_sides};
use crate::core::duration::parse_interval;
use crate::core::registry::{
    LayoutNode, Overflow, PaneBox, PaneWidth, Registry, ShellDecl, ShellMode, SourceId,
    SourceProgram, SourceSpec, TitleSource, shebang,
};
use crate::core::template::{Template, reference_at};
use crate::core::trigger::parse_trigger;
use crate::core::variables::{Tier, VariableBlock};

/// `rat watch --trigger-debounce`'s default, reused verbatim so a pane
/// and a watch behave the same when neither says otherwise.
const DEFAULT_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);
/// The shipped `rat watch` default interval.
const DEFAULT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// The dashboard's declared title: static text, a pane reference
/// (`ref="#id"`), or both — the text is then the role's fallback while
/// the referenced pane has not spoken. The parser guarantees at least
/// one of the two is present.
#[derive(Clone, PartialEq, Debug)]
pub struct TitleDecl {
    /// A string an author writes, so it is a template like any other
    /// string site. Expansion is load-time (INV-7).
    pub text: Option<Template>,
    /// The referenced pane id — the fragment with its `#` removed.
    /// A `String`, not a `Template`: an id is identity (INV-3), and
    /// the walk refuses a reference here.
    pub reference: Option<String>,
}

/// The parsed declaration. The constructor produces exactly this; ONE
/// validation path turns it into a [`Registry`].
#[derive(Clone, PartialEq, Debug, Default)]
pub struct DashboardFile {
    /// The whole dashboard's name — a different thing from any pane's
    /// own `title`, which labels one box's border.
    pub title: Option<TitleDecl>,
    /// The board's declared variables, checked but NOT evaluated —
    /// the walk never expands (INV-2). `into_registry` reads it to
    /// enforce INV-7's site rule.
    ///
    /// From `variables.rs`, NOT `dashboard_kdl.rs`: this module is
    /// imported BY the walk, so a field typed from the walk would
    /// close a cycle.
    pub variables: crate::core::variables::VariableBlock,
    pub gap: Option<usize>,
    pub row_gap: Option<usize>,
    pub defaults: PaneDecl,
    pub panes: Vec<PaneDecl>,
    /// The declared layout, by pane NAME (ids resolve at validation);
    /// absent stacks every pane in declaration order. The top-level
    /// items compose as a column, exactly as the engine's tree does.
    pub layout: Option<Vec<LayoutDecl>>,
}

/// A declared layout node: a pane by name, or a row/column of nested
/// nodes — the same recursive shape the engine's `LayoutNode` has had
/// from day one, reached by name instead of id.
#[derive(Clone, PartialEq, Debug)]
pub enum LayoutDecl {
    Pane(String),
    Row(Vec<LayoutDecl>),
    Column(Vec<LayoutDecl>),
}

impl LayoutDecl {
    /// One-cell rows and columns collapse to their cell, so the two
    /// grammars' different spellings of "a full-width pane" reach the
    /// same declaration.
    pub fn normalized(self) -> LayoutDecl {
        match self {
            LayoutDecl::Pane(name) => LayoutDecl::Pane(name),
            LayoutDecl::Row(cells) | LayoutDecl::Column(cells) if cells.len() == 1 => {
                cells.into_iter().next().expect("one cell").normalized()
            }
            LayoutDecl::Row(cells) => {
                LayoutDecl::Row(cells.into_iter().map(LayoutDecl::normalized).collect())
            }
            LayoutDecl::Column(cells) => {
                LayoutDecl::Column(cells.into_iter().map(LayoutDecl::normalized).collect())
            }
        }
    }
}

/// One pane's declaration (or the `defaults` block), tokens unparsed.
/// Every string an author writes is a [`Template`] — recorded as
/// written, its references validated at load, expanded only at its use
/// moment (INV-2).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct PaneDecl {
    pub id: Option<String>,
    /// Split argv, or one raw script string under `shell`.
    pub command: Option<Vec<Template>>,
    /// A multi-line body declared with `script`. A leading `#!` names
    /// the body's own interpreter; without one the body runs through
    /// the pane's resolved shell, exactly as unix ENOEXEC does.
    pub script: Option<Template>,
    /// `None` inherits; `Some(ShellDecl::Direct)` is an explicit
    /// `shell=#false`. The DECLARED form: a templated dialect name
    /// keeps its references and its flavor instead of being flattened
    /// into a `ShellMode::Named(String)`.
    pub shell: Option<ShellDecl>,
    /// "5s" | "never".
    pub interval: Option<Template>,
    pub trigger: Option<Vec<Template>>,
    pub trigger_debounce: Option<Template>,
    pub height: Option<u16>,
    /// "40" | "2fr" | "auto".
    pub width: Option<Template>,
    /// "keep-top" | "keep-bottom".
    pub overflow: Option<Template>,
    pub border: Option<Template>,
    /// `parse_sides` shorthand.
    pub padding: Option<Template>,
    pub title: Option<Template>,
    pub chrome: Option<bool>,
    /// Whether pane-navigation gestures may target this pane.
    pub focusable: Option<bool>,
    /// The child is long-lived: spawn it once and show its output as it
    /// arrives, rather than waiting for an exit that is not coming.
    pub live: Option<bool>,
}

impl DashboardFile {
    /// The ONE validation path: resolve defaults, parse every token,
    /// check the layout, build the registry. Every error names the
    /// pane and the fix.
    pub fn into_registry(
        self,
        bindings: &crate::core::template::Bindings,
    ) -> anyhow::Result<Registry> {
        // Not consumed yet: load-time site expansion is what reads the
        // map, and it lands with the load-time-sites task. Taking the
        // parameter NOW is what that task wrote itself against.
        let _ = bindings;
        if self.panes.is_empty() {
            bail!("no panes declared: a dashboard needs at least one pane");
        }
        if self.defaults.id.is_some() {
            bail!("an id is not a default: give each pane its own id");
        }
        if self.defaults.command.is_some() && self.defaults.script.is_some() {
            bail!(
                "defaults: declares both `command` and `script` — a pane runs \
                 one program; keep the `script` body or the `command` argv, \
                 not both"
            );
        }

        let names = self.pane_names()?;
        let mut sources = Vec::with_capacity(self.panes.len());
        let mut boxes = Vec::with_capacity(self.panes.len());
        for (decl, name) in self.panes.iter().zip(&names) {
            sources.push(resolve_source(decl, &self.defaults, &self.variables, name)?);
            boxes.push(resolve_box(decl, &self.defaults, &self.variables, name)?);
        }
        let layout = resolve_layout(self.layout.as_deref(), &names)?;
        Ok(Registry::panes(
            sources,
            boxes,
            layout,
            self.gap.unwrap_or(0),
            self.row_gap.unwrap_or(0),
        )?
        .with_title(self.resolve_title(&names)?)
        .with_diagnostics(Self::collect_diagnostics(&names)))
    }

    /// The declared title against the declared panes: a `ref` binds
    /// the FIRST pane with the id (the same first-win rule refs
    /// follow everywhere); an id nothing declares is a load error
    /// that lists what exists.
    fn resolve_title(&self, names: &[String]) -> anyhow::Result<TitleSource> {
        if let Some(TitleDecl {
            text: Some(text), ..
        }) = &self.title
        {
            // The dashboard's own title is a render decision made
            // once — a load-time site like the pane's.
            refuse_deferred_at_load_site(text, "title", false, &self.variables, "the dashboard")?;
        }
        Ok(match self.title.clone() {
            None => TitleSource::None,
            Some(TitleDecl {
                text,
                reference: None,
            }) => TitleSource::Static(
                text.expect("the parser requires text or ref")
                    .as_str()
                    .to_string(),
            ),
            Some(TitleDecl {
                text,
                reference: Some(wanted),
            }) => {
                let source = names
                    .iter()
                    .position(|id| id == &wanted)
                    .map(SourceId)
                    .ok_or_else(|| {
                        anyhow!(
                            "title ref \"#{wanted}\" names no pane — declared ids are {}",
                            names.join(", ")
                        )
                    })?;
                TitleSource::Pane {
                    source,
                    fallback: text.map(|fallback| fallback.as_str().to_string()),
                }
            }
        })
    }

    /// Every pane's name, in declaration order. Names are the file's
    /// identity for a source; `SourceId` is the engine's, and this is
    /// where the two are married.
    fn pane_names(&self) -> anyhow::Result<Vec<String>> {
        let mut names: Vec<String> = Vec::with_capacity(self.panes.len());
        for (index, decl) in self.panes.iter().enumerate() {
            let Some(id) = decl.id.as_deref() else {
                bail!("pane #{}: every pane needs an id", index + 1);
            };
            // First-win, not fatal: a duplicate only matters once a
            // ref points at it, and refs bind the first declaration.
            // The duplicate is surfaced as a diagnostic instead.
            names.push(id.to_string());
        }
        Ok(names)
    }

    /// The load-time facts worth telling but not worth failing over.
    fn collect_diagnostics(names: &[String]) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        for id in names {
            if seen.contains(&id.as_str()) {
                let line = format!("duplicate id {id:?} — refs bind to the first declaration");
                if !out.contains(&line) {
                    out.push(line);
                }
            } else {
                seen.push(id);
            }
        }
        out
    }
}

/// Every error a pane can raise names the pane first — the file may
/// have a dozen, and "invalid overflow" alone does not say which one
/// to edit.
fn at(name: &str) -> String {
    format!("pane {name:?}")
}

/// How a mode reads inside an error: the thing the author would
/// recognise from what they wrote.
fn shell_label(decl: &ShellDecl) -> String {
    match decl {
        ShellDecl::Direct => "no shell".to_string(),
        ShellDecl::Platform => "the platform shell".to_string(),
        ShellDecl::Named(name) => format!("`{}`", name.as_str()),
    }
}

/// The Phase-1 bridge from the declared shell to the resolved mode a
/// [`SourceSpec`] carries: nothing expands yet, so a templated dialect
/// name passes through as its written bytes — byte-identical to the
/// old behavior for every template-free board. Load-time expansion
/// replaces this with `ShellDecl::resolve(bindings)`; when it does,
/// the inherit-guards in `resolve_source`/`resolve_script` must move
/// from comparing DECLARED shells to comparing RESOLVED modes in the
/// same change — `shell="{{sh}}"` with `sh = "fish"` beside
/// `defaults { shell "fish" }` compares unequal as written and equal
/// as run, and the guard's own justification is about the dialect the
/// command was written for.
fn shell_mode(decl: &ShellDecl) -> ShellMode {
    match decl {
        ShellDecl::Direct => ShellMode::Direct,
        ShellDecl::Platform => ShellMode::Platform,
        ShellDecl::Named(name) => ShellMode::Named(name.as_str().to_string()),
    }
}

/// The effective value AND where it was written. Replaces the bare
/// `decl.X.or(defaults.X)` at every site the INV-7 check touches,
/// because a refusal that cannot say "inherited from `defaults`" sends
/// the author to the wrong line.
///
/// The origin bit is read ONLY by the refusal messages, so most call
/// sites discard it — that is not a licence to simplify this back to
/// `.or()`: passing a literal `false` compiles fine and merely turns
/// every inherited refusal into one that points at the wrong line.
/// Later consumers (load-time expansion's messages, the check
/// command's site report) read the same bit; check them before
/// "tidying" this away.
fn inherit<'a, T>(own: Option<&'a T>, defaults: Option<&'a T>) -> Option<(&'a T, bool)> {
    own.map(|v| (v, false))
        .or_else(|| defaults.map(|v| (v, true)))
}

/// INV-7: a site whose value is parsed into a typed thing at load must
/// be complete at load, because a hole cannot be parsed.
///
/// The check is ONE LEVEL DEEP — "is any name this string directly
/// references deferred?" — and that is sufficient only because the
/// block's tier check refused the load-time-references-deferred case
/// (INV-9 clause 2), pinned by
/// `an_effective_tier_never_outruns_its_declared_form`. If clause 2 is
/// ever relaxed to auto-promotion, this must become a transitive walk
/// on the same day; the two are a matched pair.
///
/// It reads `Template::refs` and NEVER re-scans the bytes, so a raw
/// string — whose record says it references nothing (INV-1) — is
/// accepted here without the function ever asking whether the value
/// was raw. That absence of a special case is the design working, and
/// `Template`'s deliberately missing `Deref<Target = str>` is what
/// enforces it: reaching for the bytes costs a visible `.as_str()`.
fn refuse_deferred_at_load_site(
    value: &Template,
    site: &str,
    inherited: bool,
    block: &VariableBlock,
    subject: &str,
) -> anyhow::Result<()> {
    if let Some(name) = value
        .refs()
        .iter()
        .find(|name| block.tier(name) == Some(Tier::Spawn))
    {
        let origin = if inherited {
            " (inherited from `defaults`)"
        } else {
            ""
        };
        bail!(
            "{subject}: `{site}`{origin} is read when the dashboard loads, but its \
             value references `{{{{{name}}}}}`, and `{name}` is declared `defer=#true` \
             — a deferred variable is re-derived at each spawn, and a value parsed \
             once at load has nowhere to put a hole. Drop `defer` from `{name}`, or \
             move the value into the pane's `command`, which IS read at spawn"
        );
    }
    Ok(())
}

/// INV-7's first sub-rule, exactly as narrow as its justification
/// (NEW-1): the shebang classifier reads a body's first TWO bytes, and
/// consumes the rest of the first line only when they are `#!`. So a
/// body may not BEGIN with a `{{` reference, and a `#!` first line
/// must be template-free — nothing more. A body starting with `[ "` or
/// `echo` is already classified correctly at load, so a `{{rev}}`
/// guard on line 1 is fine.
fn script_first_bytes(body: &Template, subject: &str) -> anyhow::Result<()> {
    // A raw body is literal end to end (INV-1), so nothing here can
    // apply. THIS GUARD IS `reference_at`'s DOCUMENTED PRECONDITION,
    // not a local optimization: it reads bytes, and a raw body's bytes
    // are indistinguishable from a normal one's — asking it about a
    // raw string finds a reference INV-1 says is not there.
    if !body.interpolates() {
        return Ok(());
    }
    let text = body.as_str();
    if text.starts_with("#!") {
        // The classifier reads this whole line — including the one
        // optional argument — so this whole line must be literal, and
        // nothing beyond it.
        let first = text.split('\n').next().unwrap_or(text);
        if let Some(name) = Template::extract(first).refs().first() {
            bail!(
                "{subject}: this `script` body's `#!` line references `{{{{{name}}}}}` — \
                 the `#!` line names the body's interpreter and is read at load, before \
                 any variable expands, so it must be written out in full. Write the \
                 interpreter literally, or drop the `#!` line to run the body through \
                 the pane's shell"
            );
        }
        return Ok(());
    }
    // TWO BYTES, not a line — and "begins with a REFERENCE", not
    // "begins with `{{`": the one reference scanner decides, so a
    // jq-shaped `{{ }` or an inner-whitespace `{{ rev }}` stays
    // literal text.
    if let Some((name, _)) = reference_at(text, 0) {
        bail!(
            "{subject}: this `script` body begins with `{{{{{name}}}}}` — rat reads a \
             body's first two bytes at load to see whether it names its own \
             interpreter (`#!`), and it cannot read a hole. Begin the body with \
             literal text: `echo {{{{{name}}}}}` rather than `{{{{{name}}}}}`"
        );
    }
    Ok(())
}

fn resolve_source(
    decl: &PaneDecl,
    defaults: &PaneDecl,
    block: &VariableBlock,
    id: &str,
) -> anyhow::Result<SourceSpec> {
    let defaults_shell = defaults.shell.clone().unwrap_or_default();
    let shell = decl.shell.clone().unwrap_or_else(|| defaults_shell.clone());
    // The dialect name is a string an author writes, read once at load
    // to make a dispatch decision — a load-time site like any other.
    // The row destructures `Named` and uses the one generic site
    // function; no row gets a bespoke check.
    if let Some((ShellDecl::Named(name), inherited)) =
        inherit(decl.shell.as_ref(), defaults.shell.as_ref())
    {
        refuse_deferred_at_load_site(name, "shell", inherited, block, &at(id))?;
    }
    let live = decl.live.or(defaults.live).unwrap_or(false);
    if decl.command.is_some() && decl.script.is_some() {
        bail!(
            "{}: declares both `command` and `script` — a pane runs one \
             program; keep the `script` body or the `command` argv, not both",
            at(id)
        );
    }
    // The effective program: own beats inherited, even across kinds (a
    // pane's `command` overrides a defaults `script`).
    let inherits_program = decl.command.is_none() && decl.script.is_none();
    let script = decl.script.as_ref().or_else(|| {
        decl.command
            .is_none()
            .then_some(defaults.script.as_ref())
            .flatten()
    });
    let (program, shell) = if let Some(body) = script {
        resolve_script(
            body,
            decl,
            defaults,
            &shell,
            &defaults_shell,
            inherits_program,
            id,
        )?
    } else {
        // A command string is word-split (or kept verbatim) at PARSE
        // time, under the shell mode in force where it was written —
        // and its SYNTAX belongs to that shell too. A pane that
        // inherits the defaults' program while changing `shell`, in
        // either direction or to a different shell, would run a
        // command nobody wrote for it. Fail with the fix instead.
        if inherits_program && shell != defaults_shell {
            bail!(
                "{}: inherits `command` from `defaults` but overrides `shell` — \
                 the inherited command was read and written under the defaults' \
                 shell mode ({}), not this pane's ({}); declare the pane's own \
                 `command`",
                at(id),
                shell_label(&defaults_shell),
                shell_label(&shell),
            );
        }
        let command = decl
            .command
            .clone()
            .or_else(|| defaults.command.clone())
            .filter(|words| !words.is_empty())
            .ok_or_else(|| anyhow!("{}: needs a `command` or a `script`", at(id)))?;
        // The templates ride to spawn per element, flavor and all —
        // expansion happens there, against that spawn's map (INV-2).
        (SourceProgram::Argv(command), shell)
    };

    let picked_triggers = inherit(decl.trigger.as_ref(), defaults.trigger.as_ref());
    if let Some((specs, inherited)) = &picked_triggers {
        // Each element, so a board with four triggers is told which
        // one is wrong.
        for spec in specs.iter() {
            refuse_deferred_at_load_site(spec, "trigger", *inherited, block, &at(id))?;
        }
    }
    let triggers = picked_triggers
        .map(|(specs, _)| specs.clone())
        .unwrap_or_default()
        .iter()
        .map(|spec| parse_trigger(spec.as_str()).with_context(|| at(id)))
        .collect::<anyhow::Result<Vec<_>>>()?;

    let picked_interval = inherit(decl.interval.as_ref(), defaults.interval.as_ref());
    if let Some((value, inherited)) = picked_interval {
        refuse_deferred_at_load_site(value, "interval", inherited, block, &at(id))?;
    }
    let token = picked_interval.map(|(value, _)| value.as_str());
    let interval = match (token, triggers.is_empty()) {
        (Some("never"), _) => None,
        (Some(token), _) => Some(parse_interval(token).with_context(|| at(id))?),
        (None, false) => None,
        (None, true) => Some(DEFAULT_INTERVAL),
    };

    let picked_debounce = inherit(
        decl.trigger_debounce.as_ref(),
        defaults.trigger_debounce.as_ref(),
    );
    if let Some((value, inherited)) = picked_debounce {
        refuse_deferred_at_load_site(value, "trigger-debounce", inherited, block, &at(id))?;
    }
    let debounce = match picked_debounce.map(|(value, _)| value.as_str()) {
        Some(token) => parse_interval(token).with_context(|| at(id))?,
        None => DEFAULT_DEBOUNCE,
    };

    Ok(SourceSpec {
        id: id.to_string(),
        // Verbatim in both kinds: a shell pane's script never
        // round-trips through split-then-join, and a body's bytes are
        // the author's.
        program,
        shell: shell_mode(&shell),
        interval,
        triggers,
        debounce,
        live,
    })
}

/// The script-body half of `resolve_source`: every rule a `script`
/// declaration carries, and the shell mode the spec should hold.
///
/// A SHEBANG body names its own interpreter, so its shell mode is
/// inert — an own RUNNING shell beside it is a dead declaration and is
/// refused, while `shell #false` beside it is an honest statement and
/// allowed (refusing it would leave a defaults-wide `shell #false`
/// with a shebang pane no legal spelling). A body with NO shebang runs
/// through the pane's shell, so it can never be `Direct`: an explicit
/// `#false` is refused, absence promotes to the platform's shell, and
/// the inherit-guard applies exactly as it does to a `command`.
fn resolve_script(
    body: &Template,
    decl: &PaneDecl,
    defaults: &PaneDecl,
    shell: &ShellDecl,
    defaults_shell: &ShellDecl,
    inherited: bool,
    id: &str,
) -> anyhow::Result<(SourceProgram, ShellDecl)> {
    if body.as_str().trim().is_empty() {
        bail!(
            "{}: `script` has no body — write the script inside a `\"\"\"` \
             block, or drop the key",
            at(id)
        );
    }
    script_first_bytes(body, &at(id))?;
    match shebang(body.as_str()) {
        Some(line) => {
            if let Some(own) = &decl.shell
                && own.runs_a_shell()
            {
                let spelled = match &line.arg {
                    Some(arg) => format!("{} {arg}", line.interpreter),
                    None => line.interpreter.clone(),
                };
                bail!(
                    "{}: declares `shell` and a `script` whose `#!` line \
                     already names its interpreter (`{}`) — the `#!` wins \
                     and {} would never run. Drop `shell`, or drop the \
                     `#!` line to run the body through {}",
                    at(id),
                    spelled,
                    shell_label(own),
                    shell_label(own),
                );
            }
            Ok((SourceProgram::Script(body.clone()), shell.clone()))
        }
        None => {
            let first = body.as_str().lines().next().unwrap_or("");
            if first.trim_start().starts_with("#!") {
                bail!(
                    "{}: the `#!` line must be the body's first two bytes, \
                     but this one is indented. KDL removes the closing \
                     `\"\"\"`'s indentation from every line — align the \
                     closing `\"\"\"` with the script",
                    at(id)
                );
            }
            let explicit = decl.shell.as_ref().or(defaults.shell.as_ref());
            if matches!(explicit, Some(ShellDecl::Direct)) {
                bail!(
                    "{}: `script` runs through a shell, but this pane \
                     declares `shell #false` — a body has no argv to execute \
                     directly. Drop the `shell #false`, or write the program \
                     as `command`",
                    at(id)
                );
            }
            if inherited && shell != defaults_shell {
                bail!(
                    "{}: inherits `script` from `defaults` but overrides \
                     `shell` — the inherited body was written under the \
                     defaults' shell mode ({}), not this pane's ({}); declare \
                     the pane's own `script`",
                    at(id),
                    shell_label(defaults_shell),
                    shell_label(shell),
                );
            }
            let shell = match shell {
                ShellDecl::Direct => ShellDecl::Platform,
                other => other.clone(),
            };
            Ok((SourceProgram::Script(body.clone()), shell))
        }
    }
}

fn resolve_box(
    decl: &PaneDecl,
    defaults: &PaneDecl,
    block: &VariableBlock,
    id: &str,
) -> anyhow::Result<PaneBox> {
    // `height` takes no site check: it is an integer, and integers
    // never pass the string chokepoint — there is no "what if the
    // variable isn't a number" error class at all (INV-3).
    let height = decl.height.or(defaults.height).ok_or_else(|| {
        anyhow!(
            "{}: needs a `height` — declare one on the pane or in `defaults`",
            at(id)
        )
    })?;
    let picked_width = inherit(decl.width.as_ref(), defaults.width.as_ref());
    if let Some((value, inherited)) = picked_width {
        refuse_deferred_at_load_site(value, "width", inherited, block, &at(id))?;
    }
    let width = match picked_width.map(|(value, _)| value.as_str()) {
        None | Some("auto") => PaneWidth::Weight(1),
        Some(token) => parse_width(token, id)?,
    };
    // `None` and an explicit "keep-top" are NOT the same arm here, even
    // though they resolve alike for a batch pane. A live pane is read at
    // its tail, so an undeclared overflow defaults to keep-bottom; and
    // keeping the head on one silently kills the `D` and `c` change
    // marks, so declaring it is an error rather than a preference.
    let live = decl.live.or(defaults.live).unwrap_or(false);
    let picked_overflow = inherit(decl.overflow.as_ref(), defaults.overflow.as_ref());
    if let Some((value, inherited)) = picked_overflow {
        refuse_deferred_at_load_site(value, "overflow", inherited, block, &at(id))?;
    }
    let declared_overflow = picked_overflow.map(|(value, _)| value.as_str());
    let overflow = match (declared_overflow, live) {
        (None, true) => Overflow::KeepBottom,
        (None, false) => Overflow::KeepTop,
        (Some("keep-top"), true) => bail!(
            "{}: `overflow \"keep-top\"` cannot be combined with `live`: a live \
             pane is read at its tail, and keeping the head silently disables the \
             `D` and `c` change markers. Use `overflow \"keep-bottom\"`, or drop \
             `live`.",
            at(id)
        ),
        (Some("keep-top"), false) => Overflow::KeepTop,
        (Some("keep-bottom"), _) => Overflow::KeepBottom,
        (Some(other), _) => bail!(
            "{}: unknown overflow {other:?}: expected keep-top or keep-bottom",
            at(id)
        ),
    };
    let picked_border = inherit(decl.border.as_ref(), defaults.border.as_ref());
    if let Some((value, inherited)) = picked_border {
        refuse_deferred_at_load_site(value, "border", inherited, block, &at(id))?;
    }
    let border = match picked_border.map(|(value, _)| value.as_str()) {
        None => BorderPreset::None,
        Some(token) => parse_border(token, id)?,
    };
    let picked_padding = inherit(decl.padding.as_ref(), defaults.padding.as_ref());
    if let Some((value, inherited)) = picked_padding {
        refuse_deferred_at_load_site(value, "padding", inherited, block, &at(id))?;
    }
    let padding = match picked_padding.map(|(value, _)| value.as_str()) {
        None => Sides::default(),
        Some(token) => parse_sides(token).with_context(|| at(id))?,
    };
    Ok(PaneBox {
        height,
        width,
        overflow,
        border,
        padding,
        title: {
            let picked = inherit(decl.title.as_ref(), defaults.title.as_ref());
            if let Some((value, inherited)) = picked {
                refuse_deferred_at_load_site(value, "title", inherited, block, &at(id))?;
            }
            picked.map(|(title, _)| title.as_str().to_string())
        },
        chrome: decl.chrome.or(defaults.chrome).unwrap_or(true),
        focusable: decl.focusable.or(defaults.focusable).unwrap_or(true),
    })
}

/// `"40"` = exact cells, `"2fr"` = a share of what is left, `"auto"` =
/// one share. The grammar is CSS-grid-shaped because that is the one
/// every reader already knows.
fn parse_width(token: &str, id: &str) -> anyhow::Result<PaneWidth> {
    let teach = || {
        anyhow!(
            "{}: invalid width {token:?}: expected CELLS, Nfr, or auto",
            at(id)
        )
    };
    if let Some(weight) = token.strip_suffix("fr") {
        return weight
            .trim()
            .parse()
            .map(PaneWidth::Weight)
            .map_err(|_| teach());
    }
    token.parse().map(PaneWidth::Cells).map_err(|_| teach())
}

/// `BorderPreset` has no free-function parser — it is a
/// `clap::ValueEnum`, so the flag and the file accept exactly the same
/// words, and the teaching error enumerates them from the same source.
fn parse_border(token: &str, id: &str) -> anyhow::Result<BorderPreset> {
    use clap::ValueEnum;
    BorderPreset::from_str(token, true).map_err(|_| {
        let known: Vec<String> = BorderPreset::value_variants()
            .iter()
            .filter_map(|preset| preset.to_possible_value().map(|v| v.get_name().to_string()))
            .collect();
        anyhow!(
            "{}: unknown border {token:?}: expected one of {}",
            at(id),
            known.join(", ")
        )
    })
}

/// The declared tree becomes the engine's tree: names resolve to ids,
/// and every declared pane must be placed exactly once at ANY depth —
/// an unplaced pane would run a child nobody can see; a doubly-placed
/// one would have two geometries and one output.
fn resolve_layout(items: Option<&[LayoutDecl]>, names: &[String]) -> anyhow::Result<LayoutNode> {
    let Some(items) = items else {
        return Ok(LayoutNode::Column(
            (0..names.len())
                .map(|i| LayoutNode::Pane(SourceId(i)))
                .collect(),
        ));
    };
    let mut placed = vec![false; names.len()];
    let nodes = items
        .iter()
        .map(|item| resolve_node(item, names, &mut placed))
        .collect::<anyhow::Result<Vec<_>>>()?;
    if nodes.is_empty() {
        bail!("layout is empty: name at least one pane, or omit the layout");
    }
    if let Some(index) = placed.iter().position(|seen| !seen) {
        bail!(
            "pane {:?} is declared but never placed: add it to the layout",
            names[index]
        );
    }
    Ok(LayoutNode::Column(nodes))
}

fn resolve_node(
    decl: &LayoutDecl,
    names: &[String],
    placed: &mut [bool],
) -> anyhow::Result<LayoutNode> {
    match decl {
        LayoutDecl::Pane(wanted) => {
            // The first UNPLACED pane with this id, in document order:
            // occurrences bind pairwise, so duplicate ids place both
            // panes while refs (which never place) bind the first.
            let id = names
                .iter()
                .enumerate()
                .position(|(i, name)| name == wanted && !placed[i])
                .map(SourceId)
                .ok_or_else(|| {
                    if names.iter().any(|name| name == wanted) {
                        anyhow!("layout places pane {wanted:?} more times than it is declared")
                    } else {
                        anyhow!(
                            "layout names unknown pane {wanted:?}: declared panes are {}",
                            names.join(", ")
                        )
                    }
                })?;
            placed[id.0] = true;
            Ok(LayoutNode::Pane(id))
        }
        LayoutDecl::Row(cells) => {
            if cells.is_empty() {
                bail!("layout has an empty row");
            }
            Ok(LayoutNode::Row(
                cells
                    .iter()
                    .map(|cell| resolve_node(cell, names, placed))
                    .collect::<anyhow::Result<Vec<_>>>()?,
            ))
        }
        LayoutDecl::Column(cells) => {
            if cells.is_empty() {
                bail!("layout has an empty column");
            }
            Ok(LayoutNode::Column(
                cells
                    .iter()
                    .map(|cell| resolve_node(cell, names, placed))
                    .collect::<anyhow::Result<Vec<_>>>()?,
            ))
        }
    }
}

/// Read + parse + validate. `colored` styles the syntax-error
/// snippets for the caller's stream; it changes no parse outcome.
pub fn load(
    path: &std::path::Path,
    colored: bool,
    // `Bindings` lives in `core::template`, NOT in the walk —
    // `dashboard_file.rs` may not name `dashboard_kdl.rs`, which
    // imports it.
    overrides: &crate::core::template::Bindings,
) -> anyhow::Result<(Registry, crate::core::shell::SpawnVariables)> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let file = crate::core::dashboard_kdl::parse_styled(&text, colored)
        .with_context(|| format!("in {}", path.display()))?;
    crate::core::variables::check_overrides(&file.variables, overrides)
        .with_context(|| format!("in {}", path.display()))?;
    // The runner is passed as a PARAMETER, not called from inside
    // `resolve_variables`: that inversion is what lets the graph walk
    // be tested with no subprocess, lets a checker hold a
    // `VariableBlock` while running nothing, and keeps `variables.rs`
    // and `shell.rs` from importing each other.
    let bindings = crate::core::variables::resolve_variables(&file.variables, overrides, &mut {
        |request: crate::core::variables::Derivation<'_>| {
            crate::core::shell::derive(&request).map_err(|failure| {
                let refusal = crate::core::shell::refusal(request.name, request.command, &failure);
                // Placed at the variable's declaration: the span is on
                // the block, and the document text is only in scope
                // HERE — `refusal` itself stays a pure
                // name+command+failure → message function the spawn
                // tier can reuse where there is no document to point
                // into.
                match file.variables.get(request.name) {
                    Some(variable) => {
                        let (line, column) =
                            crate::core::dashboard_kdl::line_column(&text, variable.span.start);
                        anyhow::anyhow!(
                            "{}",
                            crate::core::dashboard_kdl::syntax_error_text(
                                line,
                                column,
                                Some(&format!("{refusal:#}")),
                            )
                        )
                    }
                    None => refusal,
                }
            })
        }
    })
    .with_context(|| format!("in {}", path.display()))?;
    let variables = crate::core::shell::SpawnVariables::new(
        file.variables.clone(),
        bindings.clone(),
        overrides.clone(),
    );
    let registry = file
        .into_registry(&bindings)
        .with_context(|| format!("in {}", path.display()))?;
    Ok((registry, variables))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::core::box_model::{BorderPreset, Sides};
    use crate::core::registry::{Composition, LayoutNode, SourceId};
    use crate::core::template::Bindings;

    /// The smallest legal pane: a name, a command, a declared height.
    fn pane(id: &str, command: &[&str]) -> PaneDecl {
        PaneDecl {
            id: Some(id.to_string()),
            command: Some(command.iter().map(|&s| Template::from(s)).collect()),
            height: Some(3),
            ..PaneDecl::default()
        }
    }

    fn file(panes: Vec<PaneDecl>) -> DashboardFile {
        DashboardFile {
            panes,
            ..DashboardFile::default()
        }
    }

    fn err_of(file: DashboardFile) -> String {
        format!("{:#}", file.into_registry(&Bindings::new()).unwrap_err())
    }

    #[test]
    fn teaching_errors_name_the_kdl_spelling_not_a_toml_section() {
        // `[defaults]` is TOML section syntax, and TOML is gone. The
        // one job of a teaching error is to say what to WRITE; both of
        // these must name the bare `defaults` node.
        let mut heightless = pane("a", &["true"]);
        heightless.height = None;
        let missing_height = err_of(file(vec![heightless]));
        assert!(
            missing_height.contains("`defaults`"),
            "got {missing_height}"
        );
        assert!(
            !missing_height.contains("[defaults]"),
            "got {missing_height}"
        );

        let decl = DashboardFile {
            defaults: PaneDecl {
                command: Some(vec!["true".into()]),
                ..PaneDecl::default()
            },
            panes: vec![PaneDecl {
                id: Some("b".to_string()),
                shell: Some(ShellDecl::Platform),
                height: Some(3),
                ..PaneDecl::default()
            }],
            ..DashboardFile::default()
        };
        let shell_override = format!("{:#}", decl.into_registry(&Bindings::new()).unwrap_err());
        assert!(
            shell_override.contains("`defaults`"),
            "got {shell_override}"
        );
        assert!(
            !shell_override.contains("[defaults]"),
            "got {shell_override}"
        );
    }

    #[test]
    fn defaults_fall_through_to_every_pane() {
        let decl = DashboardFile {
            defaults: PaneDecl {
                interval: Some("30s".into()),
                border: Some("rounded".into()),
                padding: Some("0 1".into()),
                height: Some(5),
                ..PaneDecl::default()
            },
            panes: vec![
                PaneDecl {
                    height: Some(4),
                    ..pane("a", &["date"])
                },
                PaneDecl {
                    height: Some(4),
                    ..pane("b", &["date"])
                },
            ],
            ..DashboardFile::default()
        };
        // Both panes declare their own height (4 — the smallest a
        // rounded border plus the chrome row admits), so only the
        // fields the panes leave open come from `defaults`.
        let registry = decl.into_registry(&Bindings::new()).expect("registry");
        assert_eq!(registry.len(), 2);
        for id in registry.ids() {
            assert_eq!(registry.spec(id).interval, Some(Duration::from_secs(30)));
            let box_ = registry.pane(id).expect("a declared pane");
            assert_eq!(box_.border, BorderPreset::Rounded);
            assert_eq!(
                box_.padding,
                Sides {
                    top: 0,
                    right: 1,
                    bottom: 0,
                    left: 1
                }
            );
        }
    }

    #[test]
    fn a_pane_overrides_a_default() {
        let decl = DashboardFile {
            defaults: PaneDecl {
                interval: Some("30s".into()),
                height: Some(5),
                ..PaneDecl::default()
            },
            panes: vec![
                PaneDecl {
                    interval: Some("2s".into()),
                    height: Some(9),
                    ..pane("fast", &["date"])
                },
                pane("slow", &["date"]),
            ],
            ..DashboardFile::default()
        };
        let registry = decl.into_registry(&Bindings::new()).expect("registry");
        assert_eq!(
            registry.spec(SourceId(0)).interval,
            Some(Duration::from_secs(2))
        );
        assert_eq!(registry.pane(SourceId(0)).expect("pane").height, 9);
        // The pane that said nothing still gets the defaults.
        assert_eq!(
            registry.spec(SourceId(1)).interval,
            Some(Duration::from_secs(30))
        );
        assert_eq!(registry.pane(SourceId(1)).expect("pane").height, 3);
    }

    #[test]
    fn interval_never_means_no_deadline() {
        // "never" is resolved HERE and nowhere else: parse_interval's
        // grammar stays exactly as shipped, or `rat watch -n never` would
        // silently become a legal flag.
        let decl = file(vec![PaneDecl {
            interval: Some("never".into()),
            ..pane("manual", &["date"])
        }]);
        let registry = decl.into_registry(&Bindings::new()).expect("registry");
        assert_eq!(registry.spec(SourceId(0)).interval, None);
        assert!(crate::core::duration::parse_interval("never").is_err());
    }

    #[test]
    fn no_interval_and_no_trigger_defaults_to_two_seconds() {
        let registry = file(vec![pane("plain", &["date"])])
            .into_registry(&Bindings::new())
            .expect("registry");
        assert_eq!(
            registry.spec(SourceId(0)).interval,
            Some(Duration::from_secs(2))
        );
    }

    #[test]
    fn no_interval_with_a_trigger_is_trigger_only() {
        let decl = file(vec![PaneDecl {
            trigger: Some(vec!["file:./state".into()]),
            ..pane("watched", &["date"])
        }]);
        let registry = decl.into_registry(&Bindings::new()).expect("registry");
        assert_eq!(registry.spec(SourceId(0)).interval, None);
        assert_eq!(registry.spec(SourceId(0)).triggers.len(), 1);
    }

    #[test]
    fn a_pane_is_not_live_unless_it_says_so() {
        let registry = file(vec![pane("log", &["date"])])
            .into_registry(&Bindings::new())
            .expect("registry");
        assert!(!registry.spec(SourceId(0)).live);
    }

    #[test]
    fn live_is_carried_onto_the_spec() {
        let registry = file(vec![PaneDecl {
            live: Some(true),
            ..pane("log", &["date"])
        }])
        .into_registry(&Bindings::new())
        .expect("registry");
        assert!(registry.spec(SourceId(0)).live);
    }

    #[test]
    fn live_is_inheritable_from_defaults_and_overridable_per_pane() {
        let mut decl = file(vec![
            pane("a", &["date"]),
            PaneDecl {
                live: Some(false),
                ..pane("b", &["date"])
            },
        ]);
        decl.defaults.live = Some(true);
        let registry = decl.into_registry(&Bindings::new()).expect("registry");
        assert!(registry.spec(SourceId(0)).live, "inherits the default");
        assert!(
            !registry.spec(SourceId(1)).live,
            "a pane must be able to opt back out"
        );
    }

    #[test]
    fn focusability_defaults_true_and_inherits_with_an_override() {
        let mut decl = file(vec![
            pane("header", &["date"]),
            PaneDecl {
                focusable: Some(true),
                ..pane("log", &["date"])
            },
        ]);
        decl.defaults.focusable = Some(false);
        let registry = decl.into_registry(&Bindings::new()).expect("registry");
        assert!(
            !registry.pane(SourceId(0)).expect("header box").focusable,
            "the header inherits the default"
        );
        assert!(
            registry.pane(SourceId(1)).expect("log box").focusable,
            "a pane can opt back in"
        );

        let ordinary = file(vec![pane("ordinary", &["date"])])
            .into_registry(&Bindings::new())
            .expect("registry");
        assert!(
            ordinary.pane(SourceId(0)).expect("ordinary box").focusable,
            "omission preserves the existing focus behavior"
        );
    }

    #[test]
    fn a_live_pane_with_no_overflow_declared_resolves_to_keep_bottom() {
        // A live pane is read at its TAIL. The shipped `None => KeepTop`
        // default would otherwise hand every live pane the one overflow
        // refused below, making a bare `live` a load error.
        let registry = file(vec![PaneDecl {
            live: Some(true),
            ..pane("log", &["date"])
        }])
        .into_registry(&Bindings::new())
        .expect("registry");
        assert_eq!(
            registry.pane(SourceId(0)).expect("pane").overflow,
            Overflow::KeepBottom
        );
    }

    #[test]
    fn a_batch_pane_with_no_overflow_declared_still_resolves_to_keep_top() {
        // The shipped default is untouched — a live-only override, not a
        // change to what every other pane does.
        let registry = file(vec![pane("p", &["date"])])
            .into_registry(&Bindings::new())
            .expect("registry");
        assert_eq!(
            registry.pane(SourceId(0)).expect("pane").overflow,
            Overflow::KeepTop
        );
    }

    #[test]
    fn keep_top_declared_on_a_live_pane_is_refused() {
        // Under keep-top the change marks are computed and then
        // structurally discarded — the window never reaches an index at
        // or past the pane's row count — so `D` and `c` do nothing for
        // that pane and nothing says why. Silent death of two features is
        // worse than a load error.
        let err = err_of(file(vec![PaneDecl {
            live: Some(true),
            overflow: Some("keep-top".into()),
            ..pane("log", &["date"])
        }]));
        assert!(err.contains("log"), "names the pane: {err}");
        assert!(err.contains("keep-top"), "names what is wrong: {err}");
        assert!(err.contains("keep-bottom"), "names the fix: {err}");
    }

    #[test]
    fn keep_top_inherited_from_defaults_is_refused_on_a_live_pane_too() {
        // The route that would otherwise slip through: nothing on the
        // pane says keep-top, so a check reading only the pane's own
        // token passes it.
        let mut decl = file(vec![PaneDecl {
            live: Some(true),
            ..pane("log", &["date"])
        }]);
        decl.defaults.overflow = Some("keep-top".into());
        let err = err_of(decl);
        assert!(err.contains("log"), "{err}");
    }

    #[test]
    fn keep_top_stays_legal_on_a_batch_pane() {
        // The refusal is about the COMBINATION. keep-top is the shipped
        // default and must not become an error.
        let registry = file(vec![PaneDecl {
            overflow: Some("keep-top".into()),
            ..pane("p", &["date"])
        }])
        .into_registry(&Bindings::new())
        .expect("registry");
        assert_eq!(
            registry.pane(SourceId(0)).expect("pane").overflow,
            Overflow::KeepTop
        );
    }

    #[test]
    fn live_with_keep_bottom_declared_is_accepted() {
        let registry = file(vec![PaneDecl {
            live: Some(true),
            overflow: Some("keep-bottom".into()),
            ..pane("log", &["date"])
        }])
        .into_registry(&Bindings::new())
        .expect("registry");
        assert_eq!(
            registry.pane(SourceId(0)).expect("pane").overflow,
            Overflow::KeepBottom
        );
    }

    #[test]
    fn a_duplicate_id_is_first_win_not_fatal() {
        // A duplicate only matters once a ref points at it, so it is
        // a diagnostic, never a load failure. Layout cells bind the
        // first UNPLACED pane with the id, in document order — so
        // both panes still render, and refs (which never place) bind
        // the first declaration.
        let registry = file(vec![pane("git", &["date"]), pane("git", &["uptime"])])
            .into_registry(&Bindings::new())
            .expect("duplicates load");
        assert_eq!(registry.len(), 2, "both panes survive");
        assert!(
            registry
                .diagnostics()
                .iter()
                .any(|d| d.contains("duplicate id \"git\"")),
            "the duplicate is surfaced as a diagnostic: {:?}",
            registry.diagnostics()
        );
        let Composition::Panes { layout, .. } = registry.composition() else {
            panic!("panes")
        };
        let LayoutNode::Column(cells) = layout else {
            panic!("two top-level panes stack: {layout:?}")
        };
        assert_eq!(
            cells.as_slice(),
            &[LayoutNode::Pane(SourceId(0)), LayoutNode::Pane(SourceId(1))],
            "occurrences bind in document order"
        );
    }

    #[test]
    fn a_pane_placed_more_often_than_declared_is_refused() {
        let mut f = file(vec![pane("git", &["date"])]);
        f.layout = Some(vec![
            LayoutDecl::Pane("git".to_string()),
            LayoutDecl::Pane("git".to_string()),
        ]);
        let err = format!("{:#}", f.into_registry(&Bindings::new()).unwrap_err());
        assert!(err.contains("more times than"), "{err}");
    }

    #[test]
    fn an_unknown_overflow_names_the_two_values() {
        let err = err_of(file(vec![PaneDecl {
            overflow: Some("keep-middle".into()),
            ..pane("log", &["date"])
        }]));
        assert!(err.contains("log"), "{err}");
        assert!(err.contains("keep-top"), "{err}");
        assert!(err.contains("keep-bottom"), "{err}");
    }

    #[test]
    fn a_bad_trigger_spec_names_the_pane_and_the_schemes() {
        // parse_trigger already teaches the schemes; the declaration
        // layer only has to say WHICH pane wrote it.
        let err = err_of(file(vec![PaneDecl {
            trigger: Some(vec!["/tmp/state.json".into()]),
            ..pane("build", &["date"])
        }]));
        assert!(err.contains("build"), "{err}");
        assert!(err.contains("file:"), "{err}");
    }

    #[test]
    fn an_absent_layout_stacks_panes_in_declaration_order() {
        let registry = file(vec![
            pane("top", &["date"]),
            pane("middle", &["date"]),
            pane("bottom", &["date"]),
        ])
        .into_registry(&Bindings::new())
        .expect("registry");
        let Composition::Panes { layout, .. } = registry.composition() else {
            panic!("a dashboard composes panes");
        };
        assert_eq!(
            layout,
            &LayoutNode::Column(vec![
                LayoutNode::Pane(SourceId(0)),
                LayoutNode::Pane(SourceId(1)),
                LayoutNode::Pane(SourceId(2)),
            ])
        );
    }

    #[test]
    fn a_layout_naming_an_unknown_pane_lists_the_declared_names() {
        let decl = DashboardFile {
            panes: vec![pane("git", &["date"]), pane("clock", &["date"])],
            layout: Some(vec![LayoutDecl::Row(vec![
                LayoutDecl::Pane("git".to_string()),
                LayoutDecl::Pane("clok".to_string()),
            ])]),
            ..DashboardFile::default()
        };
        let err = err_of(decl);
        assert!(err.contains("clok"), "{err}");
        assert!(err.contains("clock"), "{err}");
    }

    #[test]
    fn a_shell_pane_keeps_its_script_verbatim() {
        // The script never round-trips through split-then-join, or its
        // quoting is destroyed. into_registry copies the vec.
        let script = "date +%H:%M | tr -d '\\n'";
        let decl = file(vec![PaneDecl {
            shell: Some(ShellDecl::Platform),
            command: Some(vec![script.into()]),
            ..pane("stamp", &["unused"])
        }]);
        let registry = decl.into_registry(&Bindings::new()).expect("registry");
        let spec = registry.spec(SourceId(0));
        assert_eq!(spec.shell, ShellMode::Platform);
        assert_eq!(spec.program, SourceProgram::Argv(vec![script.into()]));
    }

    /// The smallest legal script pane: a name, a body, a height.
    fn script_pane(id: &str, body: &str) -> PaneDecl {
        PaneDecl {
            id: Some(id.to_string()),
            script: Some(body.into()),
            height: Some(3),
            ..PaneDecl::default()
        }
    }

    #[test]
    fn a_pane_with_both_command_and_script_is_rejected() {
        let mut decl = pane("x", &["date"]);
        decl.script = Some("#!/bin/sh\necho hi".into());
        let err = err_of(file(vec![decl]));
        assert!(err.contains("pane \"x\""), "{err}");
        assert!(err.contains("both `command` and `script`"), "{err}");
        assert!(err.contains("not both"), "{err}");
    }

    #[test]
    fn defaults_with_both_command_and_script_are_rejected() {
        let decl = DashboardFile {
            defaults: PaneDecl {
                command: Some(vec!["date".into()]),
                script: Some("#!/bin/sh\necho hi".into()),
                height: Some(3),
                ..PaneDecl::default()
            },
            panes: vec![pane("a", &["true"])],
            ..DashboardFile::default()
        };
        let err = err_of(decl);
        assert!(err.starts_with("defaults:"), "{err}");
        assert!(err.contains("both `command` and `script`"), "{err}");
    }

    #[test]
    fn an_empty_script_body_is_rejected_with_the_block_spelling() {
        for body in ["", "   \n  "] {
            let err = err_of(file(vec![script_pane("x", body)]));
            assert!(err.contains("pane \"x\""), "{err}");
            assert!(err.contains("no body"), "{err}");
            assert!(err.contains("\"\"\""), "{err}");
        }
    }

    #[test]
    fn an_indented_shebang_is_rejected_naming_the_dedent_rule() {
        // KDL's dedent can move a `#!` off byte 0; silently taking the
        // fallback would run the body through a shell the author never
        // chose. Refuse loudly, naming the mechanism and the fix.
        let err = err_of(file(vec![script_pane("x", "  #!/bin/sh\necho hi")]));
        assert!(err.contains("pane \"x\""), "{err}");
        assert!(err.contains("first two bytes"), "{err}");
        assert!(err.contains("align the closing"), "{err}");
        // A body with no `#!` anywhere still falls back gracefully —
        // the refusal is only for a demonstrably indented shebang.
        let decl = file(vec![script_pane("y", "echo hi")]);
        assert!(decl.into_registry(&Bindings::new()).is_ok());
    }

    #[test]
    fn a_pane_with_no_program_teaches_both_spellings() {
        let mut decl = pane("x", &["unused"]);
        decl.command = None;
        let err = err_of(file(vec![decl]));
        assert!(err.contains("needs a `command` or a `script`"), "{err}");
    }

    #[test]
    fn a_panes_own_running_shell_under_a_shebang_body_is_rejected() {
        // `#true` and a name both: the declared shell would never run.
        for shell in [ShellDecl::Platform, ShellDecl::Named("fish".into())] {
            let mut decl = script_pane("x", "#!/usr/bin/env python3\nprint(1)");
            decl.shell = Some(shell);
            let err = err_of(file(vec![decl]));
            assert!(err.contains("pane \"x\""), "{err}");
            assert!(err.contains("`#!` wins"), "{err}");
            assert!(err.contains("/usr/bin/env python3"), "{err}");
            assert!(err.contains("Drop `shell`"), "{err}");
        }
        // The own key fires it even when the shebang body is INHERITED:
        // that pane's shell is exactly as dead.
        let decl = DashboardFile {
            defaults: PaneDecl {
                script: Some("#!/bin/sh\necho hi".into()),
                height: Some(3),
                ..PaneDecl::default()
            },
            panes: vec![PaneDecl {
                id: Some("x".to_string()),
                shell: Some(ShellDecl::Named("fish".into())),
                ..PaneDecl::default()
            }],
            ..DashboardFile::default()
        };
        let err = err_of(decl);
        assert!(err.contains("`#!` wins"), "{err}");
    }

    #[test]
    fn shell_false_under_a_shebangless_body_is_rejected() {
        // Own `#false`.
        let mut decl = script_pane("x", "echo hi");
        decl.shell = Some(ShellDecl::Direct);
        let err = err_of(file(vec![decl]));
        assert!(err.contains("pane \"x\""), "{err}");
        assert!(err.contains("shell #false"), "{err}");
        assert!(err.contains("`command`"), "{err}");
        // Inherited `#false` refuses the same way — resolved is
        // resolved.
        let decl = DashboardFile {
            defaults: PaneDecl {
                shell: Some(ShellDecl::Direct),
                height: Some(3),
                ..PaneDecl::default()
            },
            panes: vec![script_pane("y", "echo hi")],
            ..DashboardFile::default()
        };
        let err = err_of(decl);
        assert!(err.contains("shell #false"), "{err}");
    }

    #[test]
    fn shell_false_under_a_shebang_body_is_allowed_as_honest() {
        // No shell runs and none was promised: `#false` beside a `#!`
        // is true, and refusing it would deadlock a defaults-wide
        // `shell #false` with any shebang pane.
        let mut decl = script_pane("x", "#!/bin/sh\necho hi");
        decl.shell = Some(ShellDecl::Direct);
        let registry = file(vec![decl])
            .into_registry(&Bindings::new())
            .expect("registry");
        assert_eq!(
            registry.spec(SourceId(0)).program,
            SourceProgram::Script("#!/bin/sh\necho hi".into())
        );
        // Inherited #false likewise.
        let decl = DashboardFile {
            defaults: PaneDecl {
                shell: Some(ShellDecl::Direct),
                height: Some(3),
                ..PaneDecl::default()
            },
            panes: vec![script_pane("y", "#!/bin/sh\necho hi")],
            ..DashboardFile::default()
        };
        assert!(decl.into_registry(&Bindings::new()).is_ok());
    }

    #[test]
    fn a_script_body_inherits_from_defaults() {
        // One dispatcher body serving every pane is the advertised use:
        // the id reaches each child as RAT_PANE.
        let decl = DashboardFile {
            defaults: PaneDecl {
                script: Some("#!/bin/sh\necho $RAT_PANE".into()),
                height: Some(3),
                ..PaneDecl::default()
            },
            panes: vec![
                PaneDecl {
                    id: Some("a".to_string()),
                    ..PaneDecl::default()
                },
                PaneDecl {
                    id: Some("b".to_string()),
                    ..PaneDecl::default()
                },
            ],
            ..DashboardFile::default()
        };
        let registry = decl.into_registry(&Bindings::new()).expect("registry");
        for id in [SourceId(0), SourceId(1)] {
            assert_eq!(
                registry.spec(id).program,
                SourceProgram::Script("#!/bin/sh\necho $RAT_PANE".into())
            );
        }
    }

    #[test]
    fn an_inherited_shebangless_script_with_a_changed_shell_dialect_is_rejected() {
        // The M7.1 guard, generalized: a no-shebang body's syntax
        // belongs to the shell it was written under.
        let decl = DashboardFile {
            defaults: PaneDecl {
                shell: Some(ShellDecl::Platform),
                script: Some("echo inherited".into()),
                height: Some(3),
                ..PaneDecl::default()
            },
            panes: vec![PaneDecl {
                id: Some("x".to_string()),
                shell: Some(ShellDecl::Named("fish".into())),
                ..PaneDecl::default()
            }],
            ..DashboardFile::default()
        };
        let err = err_of(decl);
        assert!(err.contains("inherits `script`"), "{err}");
        assert!(err.contains("overrides `shell`"), "{err}");
    }

    #[test]
    fn a_shebang_body_ignores_an_inherited_shell() {
        // The defaults' shell serves the OTHER panes; this pane's `#!`
        // wins silently — nothing pane-local is dead.
        let decl = DashboardFile {
            defaults: PaneDecl {
                shell: Some(ShellDecl::Named("fish".into())),
                height: Some(3),
                ..PaneDecl::default()
            },
            panes: vec![script_pane("x", "#!/bin/sh\necho hi")],
            ..DashboardFile::default()
        };
        let registry = decl.into_registry(&Bindings::new()).expect("registry");
        assert_eq!(
            registry.spec(SourceId(0)).program,
            SourceProgram::Script("#!/bin/sh\necho hi".into())
        );
    }

    #[test]
    fn a_script_body_with_no_shell_declared_resolves_to_the_platform_shell() {
        // A shebang-less body cannot run Direct; absence promotes to
        // the platform's shell, exactly what `shell #true` means.
        let registry = file(vec![script_pane("x", "echo hi")])
            .into_registry(&Bindings::new())
            .expect("registry");
        assert_eq!(registry.spec(SourceId(0)).shell, ShellMode::Platform);
    }

    #[test]
    fn a_panes_own_command_overrides_an_inherited_script() {
        let decl = DashboardFile {
            defaults: PaneDecl {
                script: Some("#!/bin/sh\necho hi".into()),
                height: Some(3),
                ..PaneDecl::default()
            },
            panes: vec![pane("x", &["date"])],
            ..DashboardFile::default()
        };
        let registry = decl.into_registry(&Bindings::new()).expect("registry");
        assert_eq!(
            registry.spec(SourceId(0)).program,
            SourceProgram::Argv(vec!["date".into()])
        );
    }

    #[test]
    fn an_inherited_command_with_a_changed_shell_dialect_is_rejected() {
        // The syntax of the defaults' script belongs to the defaults'
        // SHELL, not just to "a shell": running it under a different
        // one executes a command nobody wrote. The error names both
        // modes so the author sees the mismatch, and the fix.
        let decl = DashboardFile {
            defaults: PaneDecl {
                shell: Some(ShellDecl::Platform),
                command: Some(vec!["printf inherited".into()]),
                height: Some(3),
                ..PaneDecl::default()
            },
            panes: vec![PaneDecl {
                id: Some("fishy".to_string()),
                shell: Some(ShellDecl::Named("fish".into())),
                ..PaneDecl::default()
            }],
            ..DashboardFile::default()
        };
        let err = format!("{:#}", decl.into_registry(&Bindings::new()).unwrap_err());
        assert!(err.contains("fishy"), "{err}");
        assert!(err.contains("the platform shell"), "{err}");
        assert!(err.contains("`fish`"), "{err}");
        assert!(err.contains("declare the pane's own `command`"), "{err}");
    }

    #[test]
    fn an_inherited_command_under_the_same_named_shell_is_accepted() {
        // Restating the defaults' own mode changes nothing the script
        // was written under.
        let decl = DashboardFile {
            defaults: PaneDecl {
                shell: Some(ShellDecl::Named("fish".into())),
                command: Some(vec!["printf inherited".into()]),
                height: Some(3),
                ..PaneDecl::default()
            },
            panes: vec![PaneDecl {
                id: Some("same".to_string()),
                shell: Some(ShellDecl::Named("fish".into())),
                ..PaneDecl::default()
            }],
            ..DashboardFile::default()
        };
        let registry = decl
            .into_registry(&Bindings::new())
            .expect("same mode inherits fine");
        assert_eq!(
            registry.spec(SourceId(0)).shell,
            ShellMode::Named("fish".to_string())
        );
    }

    #[test]
    fn the_platform_shell_is_not_the_named_sh() {
        // Platform is cmd on Windows, so "the same thing" is only true
        // on unix — the guard treats them as different modes
        // everywhere. This is the arm someone will later "fix" as a
        // bug; it is not one.
        let decl = DashboardFile {
            defaults: PaneDecl {
                shell: Some(ShellDecl::Platform),
                command: Some(vec!["printf inherited".into()]),
                height: Some(3),
                ..PaneDecl::default()
            },
            panes: vec![PaneDecl {
                id: Some("pedantic".to_string()),
                shell: Some(ShellDecl::Named("sh".into())),
                ..PaneDecl::default()
            }],
            ..DashboardFile::default()
        };
        let err = format!("{:#}", decl.into_registry(&Bindings::new()).unwrap_err());
        assert!(err.contains("pedantic"), "{err}");
        assert!(err.contains("`sh`"), "{err}");
    }

    #[test]
    fn a_named_shell_in_defaults_reaches_a_pane_with_its_own_command() {
        let decl = DashboardFile {
            defaults: PaneDecl {
                shell: Some(ShellDecl::Named("fish".into())),
                height: Some(3),
                ..PaneDecl::default()
            },
            panes: vec![PaneDecl {
                id: Some("own".to_string()),
                command: Some(vec!["date".into()]),
                ..PaneDecl::default()
            }],
            ..DashboardFile::default()
        };
        let registry = decl
            .into_registry(&Bindings::new())
            .expect("plain inheritance");
        assert_eq!(
            registry.spec(SourceId(0)).shell,
            ShellMode::Named("fish".to_string())
        );
    }

    #[test]
    fn an_inherited_command_with_an_overridden_shell_is_rejected() {
        // The defaults' command string was word-split (or kept verbatim)
        // under the DEFAULTS' shell mode at parse time; a pane that
        // flips `shell` while inheriting it would execute a
        // wrongly-shaped argv. Both directions error, teaching the fix.
        let decl = DashboardFile {
            defaults: PaneDecl {
                shell: Some(ShellDecl::Platform),
                command: Some(vec!["printf inherited".into()]),
                height: Some(3),
                ..PaneDecl::default()
            },
            panes: vec![PaneDecl {
                id: Some("plain".to_string()),
                shell: Some(ShellDecl::Direct),
                ..PaneDecl::default()
            }],
            ..DashboardFile::default()
        };
        let err = format!("{:#}", decl.into_registry(&Bindings::new()).unwrap_err());
        assert!(err.contains("plain"), "{err}");
        assert!(err.contains("shell"), "{err}");
        assert!(err.contains("command"), "{err}");

        let reverse = DashboardFile {
            defaults: PaneDecl {
                command: Some(vec!["git".into(), "status".into()]),
                height: Some(3),
                ..PaneDecl::default()
            },
            panes: vec![PaneDecl {
                id: Some("shelly".to_string()),
                shell: Some(ShellDecl::Platform),
                ..PaneDecl::default()
            }],
            ..DashboardFile::default()
        };
        assert!(reverse.into_registry(&Bindings::new()).is_err());

        // Matching modes inherit fine.
        let ok = DashboardFile {
            defaults: PaneDecl {
                command: Some(vec!["date".into()]),
                height: Some(3),
                ..PaneDecl::default()
            },
            panes: vec![
                pane("fine", &["unused"]),
                PaneDecl {
                    id: Some("inheriting".to_string()),
                    command: None,
                    ..pane("inheriting", &["unused"])
                },
            ],
            ..DashboardFile::default()
        };
        assert!(ok.into_registry(&Bindings::new()).is_ok());
    }

    #[test]
    fn a_nested_layout_resolves_rows_within_rows() {
        // A row may hold columns and a column rows, to any depth: the
        // engine's tree was recursive from day one; the declaration
        // now reaches it.
        let decl = DashboardFile {
            panes: vec![
                pane("a", &["date"]),
                pane("b", &["date"]),
                pane("c", &["date"]),
            ],
            layout: Some(vec![LayoutDecl::Row(vec![
                LayoutDecl::Column(vec![
                    LayoutDecl::Pane("a".to_string()),
                    LayoutDecl::Pane("b".to_string()),
                ]),
                LayoutDecl::Pane("c".to_string()),
            ])]),
            ..DashboardFile::default()
        };
        let registry = decl.into_registry(&Bindings::new()).expect("registry");
        let Composition::Panes { layout, .. } = registry.composition() else {
            panic!("a dashboard composes panes");
        };
        assert_eq!(
            layout,
            &LayoutNode::Column(vec![LayoutNode::Row(vec![
                LayoutNode::Column(vec![
                    LayoutNode::Pane(SourceId(0)),
                    LayoutNode::Pane(SourceId(1)),
                ]),
                LayoutNode::Pane(SourceId(2)),
            ])])
        );
    }

    #[test]
    fn nested_layout_errors_still_name_the_pane() {
        // The exactly-once and known-name rules hold at any depth.
        let dup = DashboardFile {
            panes: vec![pane("a", &["date"]), pane("b", &["date"])],
            layout: Some(vec![LayoutDecl::Row(vec![
                LayoutDecl::Pane("a".to_string()),
                LayoutDecl::Column(vec![
                    LayoutDecl::Pane("b".to_string()),
                    LayoutDecl::Pane("a".to_string()),
                ]),
            ])]),
            ..DashboardFile::default()
        };
        let err = format!("{:#}", dup.into_registry(&Bindings::new()).unwrap_err());
        assert!(
            err.contains("\"a\"") && err.contains("more times than"),
            "{err}"
        );

        let empty = DashboardFile {
            panes: vec![pane("a", &["date"])],
            layout: Some(vec![
                LayoutDecl::Pane("a".to_string()),
                LayoutDecl::Column(Vec::new()),
            ]),
            ..DashboardFile::default()
        };
        let err = format!("{:#}", empty.into_registry(&Bindings::new()).unwrap_err());
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn a_pane_without_a_height_names_the_defaults_key() {
        // Every pane's box height is DECLARED. There is no default to
        // fall back on, so the error has to teach where to put one.
        let err = err_of(file(vec![PaneDecl {
            height: None,
            ..pane("sizeless", &["date"])
        }]));
        assert!(err.contains("sizeless"), "{err}");
        assert!(err.contains("height"), "{err}");
        assert!(err.contains("defaults"), "{err}");
    }
    // ─── INV-7: the site rule ───────────────────────────────────────

    /// Parse a board and run the ONE validation path; a board that
    /// fails either stage yields its error text.
    fn load_err(text: &str) -> String {
        match crate::core::dashboard_kdl::parse_styled(text, false) {
            Err(err) => format!("{err:#}"),
            Ok(file) => format!(
                "{:#}",
                file.into_registry(&Bindings::new()).expect_err("refused")
            ),
        }
    }

    fn load_result(text: &str) -> anyhow::Result<Registry> {
        crate::core::dashboard_kdl::parse_styled(text, false)
            .expect("parses")
            .into_registry(&Bindings::new())
    }

    /// The site-rule sentence's fingerprint — what a reject must carry
    /// and an accept must not.
    const SITE_PHRASE: &str = "is read when the dashboard loads";

    fn deferred_board(site_line: &str) -> String {
        format!(
            "variables {{\n    x \"echo v\" shell=#true defer=#true\n}}\n\npane \"p\" {{\n    command \"true\"\n    height 3\n    {site_line}\n}}\n"
        )
    }

    fn load_tier_board(site_line: &str) -> String {
        format!(
            "variables {{\n    x \"echo v\" shell=#true\n}}\n\npane \"p\" {{\n    command \"true\"\n    height 3\n    {site_line}\n}}\n"
        )
    }

    #[test]
    fn every_load_time_site_refuses_a_deferred_reference_and_accepts_a_load_one() {
        // The matrix, not a checklist: each load-time site must REJECT
        // a deferred reference (naming both the variable and the site)
        // and ACCEPT a non-deferred one (whose only failures are the
        // token parser's own).
        for (site_line, site) in [
            ("trigger \"file:{{x}}\"", "trigger"),
            ("interval \"{{x}}\"", "interval"),
            ("trigger-debounce \"{{x}}\"", "trigger-debounce"),
            ("width \"{{x}}\"", "width"),
            ("overflow \"{{x}}\"", "overflow"),
            ("border \"{{x}}\"", "border"),
            ("padding \"{{x}}\"", "padding"),
            ("title \"{{x}}\"", "title"),
            ("shell \"{{x}}\"", "shell"),
        ] {
            let err = load_err(&deferred_board(site_line));
            assert!(err.contains(SITE_PHRASE), "{site} rejects: {err}");
            assert!(err.contains(&format!("`{site}`")), "{site} named: {err}");
            assert!(err.contains("`x`"), "the variable named: {err}");

            match load_result(&load_tier_board(site_line)) {
                Ok(_) => {}
                Err(err) => {
                    let err = format!("{err:#}");
                    assert!(
                        !err.contains(SITE_PHRASE),
                        "{site} accepts a load-tier reference; any failure is the \
                         token parser's own: {err}"
                    );
                }
            }
        }
        // The dashboard's own title is a site too.
        let err = load_err(
            "variables {\n    x \"echo v\" shell=#true defer=#true\n}\ntitle \"{{x}}\"\npane \"p\" {\n    command \"true\"\n    height 3\n}\n",
        );
        assert!(err.contains(SITE_PHRASE), "{err}");
        assert!(err.contains("`title`"), "{err}");

        // Rows 7 and 8 — the halves that catch an over-broad
        // implementation: `command` argv and a `script` body past the
        // classifier are SPAWN sites and must accept a deferred
        // reference.
        load_result(&deferred_board("")).expect("a bare pane loads");
        load_result(
            "variables {\n    x \"echo v\" shell=#true defer=#true\n}\n\npane \"p\" {\n    command \"echo {{x}}\"\n    shell #true\n    height 3\n}\n",
        )
        .expect("a deferred reference in command argv is the point of defer");
        load_result(
            "variables {\n    x \"echo v\" shell=#true defer=#true\n}\n\npane \"p\" {\n    script \"echo {{x}}\"\n    height 3\n}\n",
        )
        .expect("a script body past the classifier is a spawn site");
    }

    #[test]
    fn both_value_shapes_are_checked() {
        // This schema's history is that a rule holds on one spelling
        // and quietly does not hold on the other.
        let err = load_err(
            "variables {\n    x \"echo v\" shell=#true defer=#true\n}\n\npane \"p\" interval=\"{{x}}\" {\n    command \"true\"\n    height 3\n}\n",
        );
        assert!(err.contains(SITE_PHRASE), "property spelling: {err}");
        let err = load_err(&deferred_board("interval \"{{x}}\""));
        assert!(err.contains(SITE_PHRASE), "child-node spelling: {err}");
        let err = load_err(&deferred_board("trigger \"file:./a\" \"file:{{x}}\""));
        assert!(err.contains(SITE_PHRASE), "list shape: {err}");
    }

    #[test]
    fn an_inherited_value_names_the_pane_and_says_it_came_from_defaults() {
        let err = load_err(
            "variables {\n    iv \"echo v\" shell=#true defer=#true\n}\n\ndefaults {\n    interval \"{{iv}}\"\n}\n\npane \"p\" {\n    command \"true\"\n    height 3\n}\n",
        );
        assert!(err.contains("pane \"p\""), "names the pane: {err}");
        assert!(
            err.contains("inherited from `defaults`"),
            "says where to edit: {err}"
        );
        // The accept half, at this task's own boundary: the same board
        // with a load-tier variable passes the SITE check; expansion
        // equality is the load-time-expansion task's claim, not this
        // one's.
        match load_result(
            "variables {\n    iv \"echo v\" shell=#true\n}\n\ndefaults {\n    interval \"{{iv}}\"\n}\n\npane \"p\" {\n    command \"true\"\n    height 3\n}\n",
        ) {
            Ok(_) => {}
            Err(err) => {
                let err = format!("{err:#}");
                assert!(!err.contains(SITE_PHRASE), "{err}");
            }
        }
    }

    #[test]
    fn a_raw_string_holding_a_deferred_reference_is_accepted_at_a_load_typed_site() {
        // Raw-ness is decided BEFORE the site rule: a raw string's
        // record says it references nothing, so the site check finds
        // nothing — without ever asking whether the value was raw.
        // The value then fails later with the token parser's OWN error
        // where one exists; substitution never acquires opinions about
        // what the text means.
        let err = load_err(&deferred_board("interval #\"{{x}}\"#"));
        assert!(!err.contains(SITE_PHRASE), "{err}");
        assert!(
            err.contains("invalid duration"),
            "parse_interval's own error: {err}"
        );
        // `trigger` accepts any path-shaped text, so a raw one loads.
        load_result(&deferred_board("trigger #\"file:{{x}}\"#"))
            .expect("a raw trigger is literal bytes");
        // And `shell`: a dialect's refs are graph EDGES, so a raw one
        // that wrongly recorded a reference could refuse the board for
        // a cycle INV-1 says cannot exist. Given a correctly-empty
        // record, the shell row accepts.
        load_result(&deferred_board("shell #\"{{x}}\"#"))
            .expect("a raw dialect name is literal bytes");
    }

    #[test]
    fn a_load_time_variable_referencing_a_deferred_one_was_already_refused_by_the_block() {
        // The IMPLICIT chain refuses at the block's own tier check
        // (INV-9 clause 2), naming both variables — never with this
        // task's site message. This pair (with the block's
        // `an_effective_tier_never_outruns_its_declared_form`) is the
        // tripwire: if clause 2 is ever relaxed to auto-promotion, the
        // site check below must become a transitive walk the same day.
        let err = load_err(
            "variables {\n    a \"echo v\" shell=#true defer=#true\n    b \"{{a}}/x\"\n}\n\npane \"p\" {\n    command \"true\"\n    height 3\n    trigger \"file:{{b}}\"\n}\n",
        );
        assert!(
            err.contains("`b` is not deferred but references `a`, which is"),
            "clause 2's refusal, not the site rule's: {err}"
        );
        assert!(!err.contains(SITE_PHRASE), "{err}");
    }

    #[test]
    fn a_declared_deferral_chain_refuses_at_the_site_one_level_deep() {
        // The EXPLICIT chain — every link declares defer, legal per
        // INV-9 clause 3 — refuses at the site by looking only at the
        // directly-referenced name.
        let err = load_err(
            "variables {\n    a \"echo v\" shell=#true defer=#true\n    b \"echo {{a}}\" shell=#true defer=#true\n}\n\npane \"p\" {\n    command \"true\"\n    height 3\n    trigger \"file:{{b}}\"\n}\n",
        );
        assert!(err.contains(SITE_PHRASE), "{err}");
        assert!(err.contains("`b`"), "the direct reference is named: {err}");
    }

    #[test]
    fn a_script_body_beginning_with_a_reference_refuses() {
        let err = load_err(
            "variables {\n    rev \"echo v\" shell=#true\n}\n\npane \"p\" {\n    script \"{{rev}}\\necho done\"\n    height 3\n}\n",
        );
        assert!(err.contains("pane \"p\""), "{err}");
        assert!(err.contains("begins with"), "{err}");
        assert!(err.contains("`{{rev}}`"), "{err}");
    }

    #[test]
    fn a_shebang_body_with_a_template_anywhere_in_line_one_refuses() {
        // `shebang()` consumes the WHOLE first line including its one
        // optional argument, so a template in the argument is inside
        // the bytes the classifier reads.
        for body in ["#!{{sh}}\\necho hi", "#!/bin/sh {{flag}}\\necho hi"] {
            let err = load_err(&format!(
                "variables {{\n    sh \"echo v\" shell=#true\n    flag \"echo v\" shell=#true\n}}\n\npane \"p\" {{\n    script \"{body}\"\n    height 3\n}}\n"
            ));
            assert!(err.contains("`#!` line"), "{body} → {err}");
        }
    }

    #[test]
    fn a_body_beginning_with_ordinary_text_may_carry_a_template_on_line_one() {
        // NEW-1's regression test: the broad "whole first line is
        // load-time" rule wrongly refused exactly this shape — nine
        // natural bodies each grew a throwaway comment line to satisfy
        // a parser rule. Only the bytes the classifier actually reads
        // must be literal. Do not re-broaden the rule to delete this.
        load_result(
            "variables {\n    rev \"echo v\" shell=#true defer=#true\n}\n\npane \"p\" {\n    script \"[ -n \\\"{{rev}}\\\" ] || exit 0\\necho ok\"\n    height 3\n    shell #true\n}\n",
        )
        .expect("ordinary first bytes with a line-1 template load");
    }

    #[test]
    fn a_shebang_body_may_carry_templates_on_later_lines() {
        // A script body past the classifier is a spawn-time site.
        for tier in ["defer=#true", ""] {
            load_result(&format!(
                "variables {{\n    rev \"echo v\" shell=#true {tier}\n}}\n\npane \"p\" {{\n    script \"#!/bin/sh\\necho {{{{rev}}}}\"\n    height 3\n}}\n"
            ))
            .unwrap_or_else(|err| panic!("{tier:?} → {err:#}"));
        }
    }

    #[test]
    fn a_literal_double_brace_that_is_not_a_reference_is_not_a_leading_reference() {
        // "Begins with `{{`" is the wrong rule; "begins with a
        // REFERENCE" is the right one — inner whitespace and a bare
        // `{{ }` are literal text (INV-1/INV-6), which is why the check
        // consults the one reference scanner instead of respelling the
        // delimiters.
        load_result("pane \"p\" {\n    script \"{{ } | keys\"\n    height 3\n    shell #true\n}\n")
            .expect("a jq-shaped body is not a template");
        load_result(
            "pane \"p\" {\n    script \"{{ rev }} says hi\"\n    height 3\n    shell #true\n}\n",
        )
        .expect("inner whitespace is literal text");
    }

    #[test]
    fn the_word_split_still_refuses_unbalanced_quoting_in_template_text() {
        // No pane `shell`: a one-word command under a shell is kept
        // verbatim, so the SPLIT path — the one under test — is the
        // shell-less spelling.
        let err = load_err(
            "variables {\n    x \"echo v\" shell=#true\n}\n\npane \"p\" {\n    command \"echo {{x}} 'oops\"\n    height 3\n}\n",
        );
        assert!(err.contains("unbalanced quoting"), "{err}");
    }
}
