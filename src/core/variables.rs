//! The variable layer: the `variables` block as a checked map — every
//! declared variable, its form, its dependencies, and the order they
//! evaluate in — plus the two resolution modes over it: the full
//! evaluator a real load runs, and the partial evaluator a checker
//! that may execute nothing runs instead.
//!
//! Imports `template.rs` and `registry.rs` and nothing else from
//! `core`, which is what lets `dashboard_file.rs` hold a
//! [`VariableBlock`] without pointing back at the KDL walk.

use crate::core::registry::{ShellDecl, ShellMode};
use crate::core::template::{Bindings, Template};

/// WHEN a variable's value is final. Binary by INV-9: three declared
/// forms collapse to two evaluation times, and this is the one the
/// site rule (INV-7) asks about. Ordered so an effective tier is a
/// `max` over a variable and everything it references.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Tier {
    /// Final before the first frame: a constant, or a command that
    /// runs once at load. Both are `Load`, which is exactly why
    /// promotion WITHIN the load tier is silent — nothing observable
    /// changes when a constant turns out to depend on a command.
    Load,
    /// Derived again at each consuming spawn (`defer=#true`).
    Spawn,
}

/// How a variable produces its value — the DECLARED form. Three of
/// them, deliberately kept separate from [`Tier`]'s two: a type that
/// conflated them would make INV-9's promotion rule and INV-7's site
/// check disagree about the same variable.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum VarSource {
    /// A literal. No evaluation, ever — and `-v` overrides it, which
    /// is the whole of what `default` used to be (INV-5, retired).
    Constant,
    /// `shell=…`: derived once during load and memoized.
    LoadCommand(ShellDecl),
    /// `shell=… defer=#true`: derived again at each consuming spawn.
    SpawnCommand(ShellDecl),
}

impl VarSource {
    pub fn declared_tier(&self) -> Tier {
        match self {
            VarSource::Constant | VarSource::LoadCommand(_) => Tier::Load,
            VarSource::SpawnCommand(_) => Tier::Spawn,
        }
    }

    /// The shell a command variable names, or `None` for a constant.
    /// A variable NEVER consults `defaults` (INV-4): omission means
    /// static, `#true` means the platform shell full stop, and any
    /// other dialect is named by the variable itself.
    pub fn shell(&self) -> Option<&ShellDecl> {
        match self {
            VarSource::Constant => None,
            VarSource::LoadCommand(s) | VarSource::SpawnCommand(s) => Some(s),
        }
    }
}

/// One declared variable. `text` is the author's bytes — the literal
/// for a constant, the command line for a command — and it is a
/// TEMPLATE, because a variable may reference other variables (INV-3).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Variable {
    pub name: String,
    pub source: VarSource,
    pub text: Template,
    /// The effective tier: the max of this variable's declared form
    /// and every tier it references (INV-9). After clause 2's check
    /// passes it always EQUALS `source.declared_tier()` — the pin
    /// that makes the one-level site check (INV-7) sufficient.
    pub tier: Tier,
    /// The value's span in the document, for placed errors.
    pub span: std::ops::Range<usize>,
}

impl Variable {
    /// Every name this variable depends on — the ONE answer the graph,
    /// the cycle check and the tier check all read. It is the union of
    /// the names in its value or command text and the names in a
    /// templated `shell` dialect name, because a variable cannot be
    /// derived until BOTH are known. A shell name that referenced a
    /// deferred variable would otherwise slip past INV-9 clause 2
    /// entirely.
    ///
    /// Yields `&str`, not `&String`: it is the idiomatic public
    /// spelling, and it feeds `VariableBlock::tier(&str)` and
    /// `contains(&str)` directly, which is what every caller does
    /// with it.
    pub fn refs(&self) -> impl Iterator<Item = &str> + '_ {
        self.text.refs.iter().map(String::as_str).chain(
            self.source
                .shell()
                .map(ShellDecl::refs)
                .unwrap_or(&[])
                .iter()
                .map(String::as_str),
        )
    }
}

/// The `variables` block, parsed and checked: every variable plus the
/// order dependencies must be evaluated in (INV-3). Declaration order
/// is kept because it is what the file reads like; INV-3 retired it
/// as a RULE, so nothing consults it but error determinism.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct VariableBlock {
    vars: Vec<Variable>,
    /// Indices into `vars`, dependencies first.
    order: Vec<usize>,
}

impl VariableBlock {
    /// `order` must be a topological order of `vars` — dependencies
    /// first — and this is the only place that invariant can be
    /// stated, because the walk that establishes it (`classify`, in
    /// `dashboard_kdl.rs`) lives beside the KDL parser while the type
    /// lives here. That split is what keeps the module order acyclic,
    /// and the constructor is where the two halves shake hands.
    pub fn new(vars: Vec<Variable>, order: Vec<usize>) -> VariableBlock {
        VariableBlock { vars, order }
    }

    pub fn get(&self, name: &str) -> Option<&Variable> {
        self.vars.iter().find(|v| v.name == name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.vars.is_empty()
    }

    pub fn tier(&self, name: &str) -> Option<Tier> {
        self.get(name).map(|v| v.tier)
    }

    /// Evaluation order, dependencies first.
    pub fn in_order(&self) -> impl Iterator<Item = &Variable> + '_ {
        self.order.iter().map(|&i| &self.vars[i])
    }

    /// The declared set for a teaching breadcrumb — the variables'
    /// answer to `key_list` (`dashboard_kdl.rs`). SORTED, not
    /// declaration order: INV-3 made declaration order carry no
    /// meaning, and a sorted list is the one a reader can scan for a
    /// name.
    pub fn declared_list(&self) -> String {
        let mut names: Vec<&str> = self.vars.iter().map(|v| v.name.as_str()).collect();
        names.sort_unstable();
        names.join(", ")
    }
}

/// One derivation the runner is asked for. `command` arrives with the
/// variable's OWN references already expanded, so the runner never
/// needs the map; `name` is here because every failure message names
/// the variable it came from (INV-4.2).
pub struct Derivation<'a> {
    pub name: &'a str,
    pub command: &'a str,
    pub shell: &'a ShellMode,
}

/// Derive every LOAD-tier variable's final text, dependencies first
/// (INV-3). Two lines of resolution (INV-4.4): `-v` wins for the exact
/// name, otherwise a constant is its own value and a command derives
/// one. An overridden command is suppressed by never being REACHED,
/// not by a special case inside the runner.
///
/// Deferred variables are absent from the result: they have no value
/// until a spawn asks for one, and INV-7's site rule guarantees no
/// load-time site ever wanted one.
///
/// `run` is the shell runner, supplied by the caller. **The callback
/// is structural, not stylistic.** Two capabilities rest on it
/// directly: the block's topological evaluation is testable with no
/// subprocess, and `rat dashboard check` holds a `VariableBlock` while
/// running nothing at all. Both would be lost the moment this function
/// knew how to derive a value itself.
///
/// It also keeps this module and the shell runner's module
/// (`core/shell.rs`) from importing each other — the runner imports
/// `Variable` and `Derivation` from here, so a direct call back would
/// close the loop. Note what kind of claim that is: `core` is NOT
/// acyclic and never was; both modules are created by this plan,
/// neither cycles with anything today, and the cycle would be one we
/// authored for no gain. It will look removable — `shell::derive` is
/// its only production caller once it lands — and inlining it reads as
/// an obvious cleanup right up until the module graph closes.
pub fn resolve_variables(
    block: &VariableBlock,
    overrides: &Bindings,
    run: &mut dyn FnMut(Derivation<'_>) -> anyhow::Result<String>,
) -> anyhow::Result<Bindings> {
    let mut out = Bindings::new();
    for var in block.in_order() {
        if var.tier == Tier::Spawn {
            continue;
        }
        if let Some(value) = overrides.get(&var.name) {
            out.insert(var.name.clone(), value.clone());
            continue;
        }
        let text = var.text.expand(&out)?;
        let value = match var.source.shell() {
            None => text,
            Some(decl) => {
                // The dialect name is a template like any other, and
                // its references were resolved before this variable's
                // (they are graph edges — `Variable::refs`). The
                // runner therefore receives a RESOLVED `ShellMode` and
                // never learns that a template was involved.
                let shell = decl.resolve(&out)?;
                run(Derivation {
                    name: &var.name,
                    command: &text,
                    shell: &shell,
                })?
            }
        };
        out.insert(var.name.clone(), value);
    }
    Ok(out)
}

/// What a name is worth to a caller that may not run anything.
/// Named `Resolved` rather than `Value` deliberately: the KDL walk is
/// full of `kdl::KdlValue`, and a bare `Value` beside it reads as the
/// KDL one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Resolved {
    /// Final text: a constant, a `-v` override, or a chain of them.
    Known(String),
    /// Derived by running a command — or by referencing something that
    /// is. Its bytes are unknowable without executing, and no checker
    /// may guess them.
    Opaque,
}

pub type Partial = std::collections::BTreeMap<String, Resolved>;

/// Resolve as far as is possible WITHOUT running anything — the
/// second mode of [`resolve_variables`], deliberately its symmetric
/// twin: same block, same overrides, same topological order, minus the
/// runner.
///
/// Infallible, and the reason matters: every rule that can refuse a
/// board — the dependency graph, cycle detection, INV-9's tier check,
/// unknown names — fires during the PARSE, not here. A caller that got
/// a `VariableBlock` at all already survived every one of them,
/// identically to a real load, and there is nothing left for this
/// function to fail on. `rat dashboard check` needs no second
/// traversal and cannot reach a different verdict.
///
/// The rules, one per line, applied along the SAME topological order
/// `resolve_variables` walks — which is what makes opacity transitive
/// in one pass rather than a second fixpoint:
/// - a `-v` override is `Known`, even on a command variable and even
///   on a deferred one, because `-v` suppresses the command entirely
///   (INV-4.4) — so `check -v store=/x` legitimately checks MORE;
/// - any other command variable is `Opaque`;
/// - a constant is `Known` when every name it references is `Known`,
///   and `Opaque` otherwise.
///
/// Deferred variables are PRESENT here as `Opaque`, unlike
/// `resolve_variables`, which omits them: `check` must be able to say
/// a name is known-to-exist but unknowable, which is a different
/// answer from missing. (A variable with a templated `shell` is a
/// command variable by definition, so an opaque DIALECT name never
/// needs a rule of its own.)
pub fn resolve_partial(block: &VariableBlock, overrides: &Bindings) -> Partial {
    let mut out = Partial::new();
    for var in block.in_order() {
        let resolved = if let Some(value) = overrides.get(&var.name) {
            Resolved::Known(value.clone())
        } else if var.source.shell().is_some() {
            Resolved::Opaque
        } else {
            match var.text.expand_partial(&out) {
                Expanded::Known(text) => Resolved::Known(text),
                Expanded::Skipped(_) => Resolved::Opaque,
            }
        };
        out.insert(var.name.clone(), resolved);
    }
    out
}

/// What a site's expansion is worth under a partial map.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Expanded {
    /// Every reference was `Known`: these are the exact bytes the
    /// board will run, so a checker may hand them to the real token
    /// parser and report a malformed value as a real error.
    Known(String),
    /// At least one reference was `Opaque`, so the site is skipped —
    /// and these are the names that made it unknowable, in
    /// first-appearance order. A skip is visible, never silent.
    Skipped(Vec<String>),
}

impl Template {
    /// Expand against a partial map, or say exactly why it could not.
    /// Total: it never fails and never guesses.
    ///
    /// A template with no references is always `Known` — including
    /// every RAW string (INV-1), whose bytes are final by definition.
    /// So a checker can hand a raw `#"file:{{x}}"#` trigger to the
    /// real trigger parser and catch a malformed one, which is the
    /// whole point of the partial mode.
    pub fn expand_partial(&self, partial: &Partial) -> Expanded {
        // A template with no references is final bytes, full stop —
        // and this arm must come FIRST, before anything touches
        // `self.text`. It is the same early return `expand` carries,
        // for the same reason: `substitute` re-scans BYTES, and a raw
        // string's bytes are indistinguishable from a normal one's
        // (INV-1). Without this, raw `#"file:{{store}}"#` reaches
        // `substitute`, which finds a `{{store}}` INV-1 says is not
        // there, misses it in the map, and reports
        // `Skipped(["store"])` — a raw literal wrongly declared
        // unknowable.
        if self.refs.is_empty() {
            return Expanded::Known(self.text.clone());
        }
        let unknowable: Vec<String> = self
            .refs
            .iter()
            .filter(|name| !matches!(partial.get(name.as_str()), Some(Resolved::Known(_))))
            .cloned()
            .collect();
        if !unknowable.is_empty() {
            return Expanded::Skipped(unknowable);
        }
        // Every reference is present and Known, so the error arm is
        // unreachable — but it is HANDLED rather than unwrapped, so a
        // future caller cannot turn a checker into a panic.
        let known: Bindings = self
            .refs
            .iter()
            .filter_map(|name| match partial.get(name.as_str()) {
                Some(Resolved::Known(value)) => Some((name.clone(), value.clone())),
                _ => None,
            })
            .collect();
        match crate::core::template::substitute(&self.text, &known) {
            Ok(text) => Expanded::Known(text),
            Err(missing) => Expanded::Skipped(vec![missing.name]),
        }
    }
}

/// The COMMAND variables a name ultimately derives from — the roots of
/// its opacity, found by following the edges the graph already has.
///
/// `Expanded::Skipped` names what a site directly referenced, which is
/// the right answer only when that name is itself a command. For
/// `sel "{{store}}/x"` — a constant tainted by a command — the direct
/// answer is `sel`, and a report saying *"not checked, because `sel`
/// is derived by a command"* points at a line the reader can see is a
/// constant. This returns `store`, which is the line they can act on.
/// Empty for a `Known` name.
pub fn opaque_roots(block: &VariableBlock, partial: &Partial, name: &str) -> Vec<String> {
    let mut roots: Vec<String> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    // An explicit stack rather than recursion, for the same reason the
    // parse-side DFS uses one: a deep chain must not blow the stack.
    let mut stack: Vec<&str> = vec![name];
    while let Some(name) = stack.pop() {
        if seen.contains(&name) {
            continue;
        }
        seen.push(name);
        if !matches!(partial.get(name), Some(Resolved::Opaque)) {
            continue;
        }
        let Some(var) = block.get(name) else { continue };
        if var.source.shell().is_some() {
            if !roots.iter().any(|root| root == name) {
                roots.push(name.to_string());
            }
            continue;
        }
        // Depth-first in reference order: push reversed so the first
        // reference is visited first.
        let refs: Vec<&str> = var.refs().collect();
        for referent in refs.into_iter().rev() {
            stack.push(referent);
        }
    }
    roots
}
