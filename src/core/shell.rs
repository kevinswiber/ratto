//! The shell layer: the dialect table every spawn consults, and the
//! bounded runner that derives a `variables` entry's value at load
//! (and, for `defer`, at spawn).
//!
//! A variable's `shell` does not inherit while a pane's does, and the
//! asymmetry is deliberate (INV-4): variables resolve at load, before
//! any pane resolution runs at all, so there is no resolved `defaults`
//! to consult even if the design wanted one — and a shipped template's
//! variables must not change dialect because the importing board
//! declares a different `defaults`.

use crate::core::registry::ShellMode;

/// The program `ShellMode::Platform` runs — resolved at spawn time,
/// exactly as `%COMSPEC%` always was.
#[cfg(unix)]
pub(crate) fn platform_shell() -> String {
    "sh".to_string()
}

#[cfg(windows)]
pub(crate) fn platform_shell() -> String {
    std::env::var("COMSPEC").unwrap_or_else(|_| "cmd".to_string())
}

/// The platform shell's "run this string" flag. Fixed rather than
/// looked up in the dialect table: bare `--shell` (and `shell=#true`)
/// keeps its exact historical bytes even when `%COMSPEC%` names
/// something exotic — selecting different flags takes an explicit
/// `--shell=NAME`.
#[cfg(unix)]
pub(crate) fn platform_flags() -> &'static [&'static str] {
    &["-c"]
}

#[cfg(windows)]
pub(crate) fn platform_flags() -> &'static [&'static str] {
    &["/C"]
}

/// The shell's own flag(s) for "run this string": `cmd` takes `/C`,
/// PowerShell `-NoProfile -Command`, and everything else `-c` — sh,
/// bash, zsh, fish, nu, dash, ksh all agree on it. Matched on the file
/// NAME with a `.exe` stripped and lowercased, so a full path,
/// `pwsh.exe` and `PWSH.EXE` all land on the same row. An unknown name
/// is not an error: `-c` is what a program invoked with a script takes,
/// and a wrong guess surfaces as the child's own diagnostic.
///
/// `-NoProfile` because the child respawns every tick: the profile's
/// load cost would recur at the interval and anything it prints would
/// land in the frame. A script that wants the profile opts back in
/// (`--shell=pwsh -- '. $PROFILE; …'`); there would be no way to opt
/// out.
pub(crate) fn command_flags(program: &str) -> &'static [&'static str] {
    match interpreter_name(program).as_str() {
        "cmd" => &["/C"],
        "powershell" | "pwsh" => &["-NoProfile", "-Command"],
        _ => &["-c"],
    }
}

/// A program's dialect key: the file name, `.exe` stripped, lowercased.
/// `command_flags` and the interpreter tables must agree on what a
/// program is CALLED, or a full path would answer one table and not
/// the other. Windows names a program with its extension; unix never
/// does — and ONLY `.exe` is stripped, so a wrapper script `cmd.sh`
/// keeps its own row.
pub(crate) fn interpreter_name(program: &str) -> String {
    let name = std::path::Path::new(program)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    name.strip_suffix(".exe").unwrap_or(&name).to_string()
}

/// The program and flags a mode spawns, or `None` when there is no
/// shell. `Platform` NEVER consults the dialect table: bare `--shell`
/// is frozen bytes even under an exotic `COMSPEC`.
pub(crate) fn shell_invocation(mode: &ShellMode) -> Option<(String, &'static [&'static str])> {
    match mode {
        ShellMode::Direct => None,
        ShellMode::Platform => Some((platform_shell(), platform_flags())),
        ShellMode::Named(name) => Some((name.clone(), command_flags(name))),
    }
}

/// One shell, one script. The platform lives in
/// `platform_shell`/`platform_flags` and the dialect in
/// `command_flags`; the one `#[cfg]` here is QUOTING, a concern only
/// Windows has — unix hands the script over as a real argv element
/// and never serializes it into a command line.
pub(crate) fn shell_command(program: &str, flags: &[&str], script: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    cmd.args(flags);
    // cmd.exe does not parse the `\"` escapes MSVCRT-style argument
    // quoting writes, so a body handed to `/C` must be the author's
    // bytes verbatim — exactly what typing it at a prompt would send.
    // `/C` marks the dialect: only cmd (and the Windows platform
    // shell, which is cmd) takes it. The other dialects keep the
    // quoted form their parsers understand.
    #[cfg(windows)]
    if matches!(flags, ["/C"]) {
        use std::os::windows::process::CommandExt;
        cmd.raw_arg(script);
        return cmd;
    }
    cmd.arg(script);
    cmd
}

/// How long a variable's command may take before the wait gives up.
///
/// Not declarable, and that is deliberate — a bound nobody has hit
/// does not need a knob, and adding one later is additive where
/// removing one is not.
///
/// Five seconds is chosen against what these commands actually are.
/// The motivating case is `git rev-parse --git-common-dir`,
/// milliseconds warm and a second or two on a cold network
/// filesystem. The cost of being too generous is asymmetric and worth
/// naming: at LOAD it hangs startup with no UI to explain why
/// (INV-4.2), and under `defer` it stalls the frame loop once per
/// consuming spawn, because that derivation runs on the loop thread
/// inside the spawn-command builder.
pub(crate) const DERIVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The value's byte ceiling. A variable's value is a path, a revision,
/// a flag string — 64 KiB is far past any of those, and past it the
/// failure is LOUD (`Truncated`), never a silently dropped value.
///
/// A BYTE cap, not a line cap: a value is one line in every motivating
/// case, so a line count would let a pathological single line through
/// while rejecting a harmless 65-line one. The number was picked, not
/// measured — what is under test is that a bound exists and that
/// overflowing it is loud, so a real measurement may change it freely.
const VALUE_CAP: usize = 64 * 1024;

/// How much stderr an error message echoes: a bounded, human-readable
/// diagnostic, not a transcript — the error prints to a terminal with
/// no scrollback control. Picked, not measured, like `VALUE_CAP`.
const STDERR_ECHO_LINES: usize = 5;

/// Every way a derivation can fail. Declared HERE because this module
/// both compiles `derive`'s signature and constructs every variant;
/// the teaching SENTENCE each variant turns into belongs to the
/// error-rendering layer, which is the one place the wording lives.
#[derive(Debug)]
pub(crate) enum DerivationFailure {
    /// A reference the map could not fill — unreachable after load
    /// validation, handled so the type is total.
    Expansion(crate::core::template::MissingVariable),
    /// A `shell="{{d}}"` dialect name that expanded to nothing.
    EmptyShell(crate::core::registry::EmptyShellName),
    /// The SHELL failed to start. `program` is the shell, not the
    /// command: under `shell="fish"` the command text is
    /// `git rev-parse …` while the missing program is `fish`, and only
    /// the resolution point knows that name — it is captured there and
    /// carried, never re-derived.
    Spawn {
        program: String,
        source: std::io::Error,
    },
    /// The command ran and failed. `None` means signalled.
    Exit { code: Option<i32>, stderr: String },
    /// stdout overflowed the byte ceiling.
    Truncated { cap: usize },
    /// The command printed nothing (or only whitespace).
    Empty,
    /// The bounded wait gave up.
    Timeout(std::time::Duration),
}

impl From<crate::core::template::MissingVariable> for DerivationFailure {
    fn from(missing: crate::core::template::MissingVariable) -> Self {
        DerivationFailure::Expansion(missing)
    }
}

impl From<crate::core::registry::EmptyShellName> for DerivationFailure {
    fn from(empty: crate::core::registry::EmptyShellName) -> Self {
        DerivationFailure::EmptyShell(empty)
    }
}

// A placeholder rendering: enough for a load error to name what went
// wrong. The teaching wording per variant is owned by the failure-
// rendering layer and replaces this text, not this mechanism.
impl std::fmt::Display for DerivationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DerivationFailure::Expansion(missing) => write!(f, "{missing}"),
            DerivationFailure::EmptyShell(empty) => write!(f, "{empty}"),
            DerivationFailure::Spawn { program, source } => {
                write!(f, "could not start `{program}`: {source}")
            }
            DerivationFailure::Exit { code, stderr } => match code {
                Some(code) => write!(f, "the command exited with status {code}: {stderr}"),
                None => write!(f, "the command was killed by a signal: {stderr}"),
            },
            DerivationFailure::Truncated { cap } => {
                write!(f, "the command printed more than {cap} bytes")
            }
            DerivationFailure::Empty => write!(f, "the command printed nothing"),
            DerivationFailure::Timeout(bound) => {
                write!(f, "the command did not finish within {bound:?}")
            }
        }
    }
}

impl std::error::Error for DerivationFailure {}

/// Strip the trailing run of newlines, removing one `\r` before each —
/// a CRLF terminator is one terminator. Nothing else is touched:
/// leading/trailing spaces and every INTERIOR byte survive, which is
/// why the value is read as bounded raw bytes rather than through a
/// display-path line accumulator that normalizes interior CRLFs.
fn trim_value(raw: &str) -> &str {
    let mut value = raw;
    while let Some(stripped) = value.strip_suffix('\n') {
        value = stripped.strip_suffix('\r').unwrap_or(stripped);
    }
    value
}

/// Classify a successful exit's stdout into a value. Order matters:
/// overflow is checked BEFORE emptiness, or an over-long value whose
/// bytes were dropped would be misreported as "printed nothing" — a
/// confidently wrong diagnosis (INV-4.2's failure class).
fn classify_value(stdout: Vec<u8>, truncated: bool) -> Result<String, DerivationFailure> {
    if truncated {
        return Err(DerivationFailure::Truncated { cap: VALUE_CAP });
    }
    let raw = String::from_utf8_lossy(&stdout);
    let value = trim_value(&raw);
    // A command printing only whitespace derived nothing — the same
    // refusal an all-whitespace shell name gets, for the same reason:
    // almost always something that expanded to nothing.
    if value.trim().is_empty() {
        return Err(DerivationFailure::Empty);
    }
    Ok(value.to_string())
}

/// Read a pipe to EOF, keeping at most `cap` bytes. Always to EOF —
/// an early exit would let the child block on a full pipe — and the
/// overflow is a FLAG, not a truncation the caller could mistake for
/// the whole value.
fn bounded_read<R: std::io::Read>(pipe: Option<R>, cap: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut truncated = false;
    if let Some(mut pipe) = pipe {
        let mut buf = [0u8; 8 * 1024];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if !truncated && out.len() + n > cap {
                        truncated = true;
                        out.clear();
                    }
                    if !truncated {
                        out.extend_from_slice(&buf[..n]);
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    }
    (out, truncated)
}

/// Run one variable's command and return its value.
///
/// `Derivation` is the variable layer's type, and its shape is INV-4's
/// pin made structural: a name, a command, and the variable's OWN
/// `ShellMode`. **There is no `defaults` here and there never may be**
/// — the runner cannot consult a block it is never handed, which is
/// what keeps a shipped template's variables from changing dialect
/// under a consuming board's `defaults { shell "fish" }`.
///
/// The conventions are `core::child.rs`'s, deliberately: both pipes
/// drain CONCURRENTLY (a serial reader deadlocks on a child that fills
/// both), the readers always run to EOF, and stdin is null because the
/// loop owns the keyboard.
///
/// It does NOT go through `child.rs`. That module runs a PANE's tick:
/// it parks in a `ChildSlot`, posts a `TickOutcome`, and has no
/// deadline. A derivation is synchronous, bounded, and has no source
/// id. Sharing the path would mean teaching `run_parked` a timeout it
/// exists without.
pub(crate) fn derive(
    request: &crate::core::variables::Derivation<'_>,
) -> Result<String, DerivationFailure> {
    derive_with(request, DERIVE_TIMEOUT)
}

fn derive_with(
    request: &crate::core::variables::Derivation<'_>,
    timeout: std::time::Duration,
) -> Result<String, DerivationFailure> {
    use crate::core::retain::{Keep, Retention, read_all};

    let Some((program, flags)) = shell_invocation(request.shell) else {
        // `Direct` is unreachable for a variable: the grammar refuses
        // `shell=#false` on one, and omitting `shell` means CONSTANT.
        // A defensive failure rather than a panic — a diagnostic path
        // must never take a user's board down with it.
        debug_assert!(false, "a variable's shell is never Direct");
        return Err(DerivationFailure::Spawn {
            program: String::new(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a variable without a shell derives nothing",
            ),
        });
    };
    let mut command = shell_command(&program, flags, request.command);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // The shell's name is captured HERE, the one place that knows it:
    // neither the command text nor the io::Error does.
    let mut child = command.spawn().map_err(|source| DerivationFailure::Spawn {
        program: program.clone(),
        source,
    })?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    // The child stays OWNED BY THIS THREAD: the kill on the timeout
    // path needs `&mut Child`, and ownership is what makes that path
    // lock-free — the workers below only ever hold the pipes.
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::Builder::new()
        .name("rat-derive".into())
        .spawn(move || {
            // stderr on its own thread, stdout inline: both pipes drain
            // at once, or a child filling both deadlocks the wait.
            let err_reader = std::thread::Builder::new()
                .name("rat-derive-stderr".into())
                .spawn(move || {
                    read_all(
                        stderr,
                        Retention {
                            max_lines: STDERR_ECHO_LINES,
                            keep: Keep::Top,
                        },
                    )
                });
            let value = bounded_read(stdout, VALUE_CAP);
            let (mut err_lines, dropped) =
                err_reader.map_or_else(|_| (Vec::new(), 0), |h| h.join().unwrap_or_default());
            if dropped > 0 {
                // The echo is bounded; say so rather than ending
                // mid-diagnostic as if that were all the child said.
                err_lines.push("…".as_bytes().to_vec());
            }
            let _ = tx.send((value, err_lines));
        });
    let drained = reader.ok().and_then(|_| rx.recv_timeout(timeout).ok());
    let Some(((stdout, truncated), err_lines)) = drained else {
        // The bound fired: kill the child and reap it (quick once
        // killed). The reader threads are NOT joined here — a
        // grandchild holding the pipe would make that join unbounded,
        // which would be the bound's own defect; they exit at EOF.
        let _ = child.kill();
        let _ = child.wait();
        return Err(DerivationFailure::Timeout(timeout));
    };
    // Both pipes hit EOF, so the wait is a reap, not a stall.
    let status = child.wait().map_err(|source| DerivationFailure::Spawn {
        program: program.clone(),
        source,
    })?;
    if !status.success() {
        let stderr = err_lines
            .iter()
            .map(|line| String::from_utf8_lossy(line).into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        return Err(DerivationFailure::Exit {
            code: status.code(),
            stderr,
        });
    }
    // stderr on the success path is DISCARDED: git and friends print
    // hints to stderr while succeeding, and a variable's value is its
    // stdout. Do not "fix" this by echoing it anywhere.
    classify_value(stdout, truncated)
}

/// A variable's errors name it first, for the same reason a pane's
/// do: a block may hold a dozen, and "the command exited 128" alone
/// does not say which line to edit.
fn at_variable(name: &str) -> String {
    format!("variable {name:?}")
}

/// The ONE fallback spelling every message teaches. A constant so two
/// messages cannot drift into teaching different fixes for the same
/// fix.
const FALLBACK: &str =
    "write the fallback where the shell already has the operator: `… 2>/dev/null || echo -`";

/// The child's stderr as an indented block beneath the sentence, or
/// nothing when it printed none. Multi-line messages are house-legal:
/// everything prints through `rat: {err:#}` with the head line first.
fn echo(stderr: &str) -> String {
    if stderr.trim().is_empty() {
        return String::new();
    }
    let mut out = String::from("\n  the command's own diagnostic:");
    for line in stderr.lines() {
        out.push_str("\n    ");
        out.push_str(line);
    }
    out
}

/// The load-tier boundary: every way a derivation can fail, as a
/// teaching load error in the house shape — the subject first, what
/// happened, why it matters, and the fix spelled out. The spawn tier
/// renders the SAME words through its own adapter; neither tier
/// re-formats a sentence of its own.
pub(crate) fn refusal(name: &str, command: &str, failure: &DerivationFailure) -> anyhow::Error {
    use anyhow::anyhow;
    let at = at_variable(name);
    match failure {
        // Unreachable after load-time name validation; worded anyway
        // because the type is total.
        DerivationFailure::Expansion(missing) => {
            anyhow!("{at}: the command references {missing} — declare it in the `variables` block")
        }
        DerivationFailure::EmptyShell(empty) => anyhow!(
            "{at}: `shell` names nothing — the dialect (written as {declared:?}) came back \
             empty; name the program, or use `shell=#true` for the platform's shell",
            declared = empty.declared,
        ),
        // The SHELL, never the command: under `shell="fish"` the
        // command text is `git rev-parse …` while the missing program
        // is `fish`, and presenting the command as the program sends
        // the reader hunting for the wrong thing.
        DerivationFailure::Spawn { program, source } => anyhow!(
            "{at}: could not start `{program}` ({source}) — that is the shell the \
             command runs through, not the command itself; name a shell that exists, \
             or use `shell=#true` for the platform's shell"
        ),
        DerivationFailure::Exit { code, stderr } => anyhow!(
            "{at}: the command exited {how} — a command variable's output IS its \
             value, so a failure has no value to give: `{command}`. If a fallback is \
             meant, {FALLBACK}{echoed}",
            how = match code {
                Some(code) => code.to_string(),
                None => "on a signal".to_string(),
            },
            echoed = echo(stderr),
        ),
        DerivationFailure::Truncated { cap } => anyhow!(
            "{at}: the command printed more than {cap} bytes — a value is a path or \
             a token, not a listing; narrow the command (`… | head -1`): `{command}`"
        ),
        DerivationFailure::Empty => anyhow!(
            "{at}: the command printed nothing — a silently empty value would expand \
             into a plausible-looking wrong path that fails much later, which is worse \
             than not starting: `{command}`. If empty is meaningful here, {FALLBACK}"
        ),
        DerivationFailure::Timeout(bound) => anyhow!(
            "{at}: the command was still running after {bound} — a once-at-load \
             variable runs before the first frame, so a hanging command hangs startup \
             with no UI to explain it: `{command}`. Bound the command itself \
             (`timeout 2 …`), or derive the value another way",
            bound = crate::core::duration::brief_duration(*bound),
        ),
    }
}

/// A deferred derivation failure, as the pane's body.
///
/// `pub(crate)`, and its ONE caller is `resolve_for_spawn` — not the
/// watch engine, which receives an already-worded `io::Error` and only
/// `?`s it.
///
/// DELEGATES to `refusal` — never re-`format!`s a sentence. A `defer`
/// failure and a load failure describe the SAME event and must not
/// describe it differently. What changes is only where the text lands:
/// there is no load left to refuse, so it fails that pane's spawn
/// instead, through the existing spawn-error path.
pub(crate) fn spawn_refusal(
    name: &str,
    command: &str,
    failure: &DerivationFailure,
) -> std::io::Error {
    std::io::Error::other(format!("{:#}", refusal(name, command, failure)))
}

/// One deferred variable, resolved. Typed failure, so the variants
/// stay unit-testable; `command` is the EXPANDED command when
/// expansion got that far, and the raw template when it did not — the
/// message echoes whichever is the truer thing to show the author.
struct Failed {
    command: String,
    failure: DerivationFailure,
}

fn derive_one(
    var: &crate::core::variables::Variable,
    so_far: &crate::core::template::Bindings,
) -> Result<String, Failed> {
    let command = var.text.expand(so_far).map_err(|missing| Failed {
        command: var.text.as_str().to_string(),
        failure: missing.into(),
    })?;
    let Some(decl) = var.source.shell() else {
        // Unreachable: a deferred variable IS a command variable.
        debug_assert!(false, "a deferred variable always carries a shell");
        return Err(Failed {
            command,
            failure: DerivationFailure::Empty,
        });
    };
    // The dialect is a template like any other, resolved against the
    // map built so far — its references joined the dependency set, so
    // they are already resolved by the time this variable's turn comes.
    let shell = match decl.resolve(so_far) {
        Ok(shell) => shell,
        Err(empty) => {
            return Err(Failed {
                command,
                failure: empty.into(),
            });
        }
    };
    derive(&crate::core::variables::Derivation {
        name: &var.name,
        command: &command,
        shell: &shell,
    })
    .map_err(|failure| Failed { command, failure })
}

/// The `Bindings` one spawn expands against: the load map, plus every
/// deferred variable this spawn will consume, each derived exactly
/// once.
///
/// `needed` is the union of each about-to-expand string's recorded
/// references. Names outside it are NOT derived — "per CONSUMING
/// spawn" (INV-4.3) means a pane that does not reference a deferred
/// variable never pays for it. The walk covers the transitive closure
/// of `needed` under `Variable::refs()` — the union of a variable's
/// text and shell references, never the text's alone — so a deferred
/// variable depending on another deferred variable, or on one only its
/// DIALECT names, sees a resolved value (INV-9 clause 3).
///
/// The load map is the BASE: non-deferred names were resolved once at
/// load and are copied through untouched, never re-derived. Deferred
/// names are absent from it by construction, so there is no stale
/// value to accidentally shadow. `-v` overrides apply here too, by
/// exact name, before any command is reached — and they do NOT change
/// a variable's tier.
///
/// ONE call per spawn is what makes INV-9's per-spawn memoization
/// true: the map is built once and reused for every string in that
/// spawn, so a name referenced twice runs its command once and both
/// occurrences agree. The failure comes out as an already-worded
/// `io::Error` because this walk covers SEVERAL variables — by the
/// time a bare failure reached a caller, which variable failed would
/// be gone, so the name is attached here, where it is in scope,
/// exactly as the load tier attaches it inside its own callback.
pub(crate) fn resolve_for_spawn(
    block: &crate::core::variables::VariableBlock,
    load: &crate::core::template::Bindings,
    overrides: &crate::core::template::Bindings,
    needed: &[&str],
) -> Result<crate::core::template::Bindings, std::io::Error> {
    use crate::core::variables::Tier;
    // The common case first: a spawn that references no deferred
    // variable takes a path that cannot fail beyond a clone.
    let mut wanted: Vec<&str> = Vec::new();
    let mut stack: Vec<&str> = needed.to_vec();
    while let Some(name) = stack.pop() {
        if wanted.contains(&name) {
            continue;
        }
        let Some(var) = block.get(name) else { continue };
        if var.tier != Tier::Spawn {
            continue;
        }
        wanted.push(name);
        // `Variable::refs()`, never the text's refs alone: a templated
        // dialect's references are dependencies too.
        stack.extend(var.refs());
    }
    let mut out = load.clone();
    if wanted.is_empty() {
        return Ok(out);
    }
    // Dependencies first: the block's topological order, filtered to
    // the deferred names this spawn actually consumes.
    let in_this_spawn: Vec<&crate::core::variables::Variable> = block
        .in_order()
        .filter(|var| wanted.contains(&var.name.as_str()))
        .collect();
    for var in in_this_spawn {
        if let Some(value) = overrides.get(&var.name) {
            // Suppressed by never being REACHED, at this tier too.
            out.insert(var.name.clone(), value.clone());
            continue;
        }
        match derive_one(var, &out) {
            Ok(value) => {
                out.insert(var.name.clone(), value);
            }
            Err(bad) => return Err(spawn_refusal(&var.name, &bad.command, &bad.failure)),
        }
    }
    Ok(out)
}

/// Everything a SPAWN needs to expand a template, carried from the
/// load that produced it to the loop that spawns children.
///
/// `Default` is empty and empty is the whole of today's behavior:
/// `rat watch` builds one and every board with no `variables` block
/// builds one. The spawn site's fast-path gate is finer-grained than
/// this value — a PROGRAM with no recorded references skips the
/// variable machinery entirely, whichever board it sits on.
///
/// The fields are PRIVATE and the constructor is the only way in: a
/// `pub` load field would invite a caller to expand against the load
/// map directly, skipping the deferred tier entirely — silently
/// correct on boards with no `defer`, and wrong on exactly the boards
/// this exists for.
#[derive(Clone, Debug, Default)]
pub(crate) struct SpawnVariables {
    /// The parsed block — only it knows a name's tier.
    block: crate::core::variables::VariableBlock,
    /// The memoized load-tier values — the base of every spawn map,
    /// copied through and NEVER re-derived.
    load: crate::core::template::Bindings,
    /// The `-v` values, carried rather than pre-merged into `load`:
    /// merging would hide an override of a DEFERRED name from
    /// `resolve_for_spawn`, the only place that suppression can happen
    /// — a deferred name has no load-time value to overwrite.
    overrides: crate::core::template::Bindings,
}

impl SpawnVariables {
    /// The ONE constructor, taking all three parts at once so a
    /// half-built context is unrepresentable: there is no setter, so
    /// the load map can never be attached without the block that
    /// classifies its names.
    pub(crate) fn new(
        block: crate::core::variables::VariableBlock,
        load: crate::core::template::Bindings,
        overrides: crate::core::template::Bindings,
    ) -> SpawnVariables {
        SpawnVariables {
            block,
            load,
            overrides,
        }
    }

    /// The map one spawn expands against. Already worded on failure:
    /// the identity of a failed variable is attached inside the
    /// resolver's own walk, so this hands back an error ready for the
    /// spawn-error path.
    pub(crate) fn for_spawn(
        &self,
        needed: &[&str],
    ) -> Result<crate::core::template::Bindings, std::io::Error> {
        resolve_for_spawn(&self.block, &self.load, &self.overrides, needed)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::core::registry::{ShellDecl, ShellMode};
    use crate::core::template::{Bindings, Template};
    use crate::core::variables::{
        Derivation, Tier, VarSource, Variable, VariableBlock, resolve_variables,
    };

    fn load_command(name: &str, command: &str) -> Variable {
        Variable {
            name: name.to_string(),
            source: VarSource::LoadCommand(ShellDecl::Platform),
            text: Template::extract(command),
            tier: Tier::Load,
            span: 0..0,
        }
    }

    /// The platform shell's spelling of "append one `x` to COUNTER,
    /// then print OUT".
    fn append_then_print(counter: &std::path::Path, out: &str) -> String {
        #[cfg(unix)]
        {
            format!("printf x >> {}; printf {}", counter.display(), out)
        }
        #[cfg(windows)]
        {
            format!("echo x>> \"{}\" & echo {}", counter.display(), out)
        }
    }

    #[test]
    fn a_command_variables_command_runs_exactly_once_per_load() {
        // "Ran once" is proven child-side (the counter file) as well as
        // by our own bookkeeping (the callback count).
        let dir = tempfile::tempdir().expect("tempdir");
        let counter = dir.path().join("runs");
        let block = VariableBlock::new(
            vec![load_command(
                "store",
                &append_then_print(&counter, "derived"),
            )],
            vec![0],
        );
        let mut calls = 0;
        let bindings =
            resolve_variables(&block, &Bindings::new(), &mut |request: Derivation<'_>| {
                calls += 1;
                derive(&request).map_err(|failure| anyhow::anyhow!("{failure}"))
            })
            .expect("the derivation succeeds");
        assert_eq!(calls, 1);
        assert_eq!(bindings.get("store").map(String::as_str), Some("derived"));
        let counted = std::fs::read_to_string(&counter).expect("the command ran");
        assert_eq!(counted.matches('x').count(), 1);
    }

    #[test]
    fn a_trailing_newline_is_trimmed_and_nothing_else_is() {
        // The trim rule is exactly as narrow as its justification.
        for (raw, want) in [
            // INV-4.2 — a `\n` inside a `trigger` fails silently and
            // unreadably.
            ("/some/path\n", "/some/path"),
            // A CRLF terminator is one terminator.
            ("/some/path\r\n", "/some/path"),
            // Matches `$( )`, which strips ALL trailing newlines.
            ("/some/path\n\n\n", "/some/path"),
            // Leading and trailing spaces survive — trimming them would
            // silently alter a value nobody asked us to edit.
            ("  spaced  ", "  spaced  "),
            // An interior newline is content; only the trailing run goes.
            ("line one\nline two\n", "line one\nline two"),
            // The interior CRLF survives — only the trailing
            // terminator's `\r` goes. This is the row a display-path
            // accumulator would normalize, and why the value is read as
            // bounded raw bytes.
            ("line one\r\nline two\r\n", "line one\r\nline two"),
        ] {
            assert_eq!(trim_value(raw), want, "{raw:?}");
        }
        // Past the byte ceiling it is a LOUD failure, never Empty: an
        // over-long single-line value reported as "printed nothing"
        // would be a confidently wrong diagnosis about a silently
        // dropped value.
        assert!(matches!(
            classify_value(Vec::new(), true),
            Err(DerivationFailure::Truncated { .. })
        ));
        assert!(matches!(
            classify_value(Vec::new(), false),
            Err(DerivationFailure::Empty)
        ));
        // A command printing only whitespace derived nothing.
        assert!(matches!(
            classify_value(b"   \n".to_vec(), false),
            Err(DerivationFailure::Empty)
        ));
    }

    #[test]
    fn a_variable_wanting_another_shell_names_it_itself() {
        // Asserted on the resolved (program, flags) pair rather than by
        // running `fish`, so the test is portable.
        let (program, flags) = shell_invocation(&ShellMode::Named("fish".into())).expect("a shell");
        assert_eq!(program, "fish");
        assert_eq!(flags, ["-c"]);
        // `shell=#true` is the platform pair — frozen bytes, never the
        // dialect table.
        let (program, flags) = shell_invocation(&ShellMode::Platform).expect("a shell");
        assert_eq!(program, platform_shell());
        assert_eq!(flags, platform_flags());
    }

    #[test]
    fn a_deferred_variable_is_not_run_and_is_absent_from_the_load_map() {
        let mut deferred = load_command("head", "echo x");
        deferred.source = VarSource::SpawnCommand(ShellDecl::Platform);
        deferred.tier = Tier::Spawn;
        let block = VariableBlock::new(vec![deferred], vec![0]);
        let bindings = resolve_variables(&block, &Bindings::new(), &mut |_| {
            panic!("a deferred variable is never derived at load")
        })
        .expect("resolves");
        // The absence is the second half: a deferred hole reaching a
        // load-time site reads as an unknown name, never a wrong value.
        assert!(!bindings.contains_key("head"));
    }

    #[test]
    fn a_hung_command_is_bounded_and_its_child_is_killed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("marker");
        // The marker is written AFTER the sleep, so a bound that
        // returns while leaving the child running shows up as the
        // marker appearing later. The sleeper's own streams are
        // redirected away so the orphan (the kill lands on the shell)
        // does not hold our pipes open for its remaining nap — the
        // readers exit at EOF either way, but a held pipe reads as a
        // leaked handle in the test runner.
        #[cfg(unix)]
        let command = format!("sleep 2 > /dev/null 2>&1; echo x > {}", marker.display());
        #[cfg(windows)]
        let command = format!(
            "ping -n 3 127.0.0.1 > NUL 2>&1 & echo x > \"{}\"",
            marker.display()
        );
        let bound = Duration::from_millis(200);
        let request = Derivation {
            name: "hung",
            command: &command,
            shell: &ShellMode::Platform,
        };
        let started = Instant::now();
        let outcome = derive_with(&request, bound);
        let elapsed = started.elapsed();
        assert!(
            matches!(outcome, Err(DerivationFailure::Timeout(_))),
            "a hung command times out"
        );
        // Never a tight equality; CI timing is not a contract.
        assert!(elapsed < bound * 3, "bounded: {elapsed:?}");
        // The strong half: the child is DEAD. The shell was killed
        // before the sleep finished, so the marker never appears within
        // a generous settle window. (A shell that forks instead of
        // execs could leave a grandchild alive holding the pipes — the
        // sleeper is a direct child here, so the kill provably lands.)
        std::thread::sleep(Duration::from_secs(3));
        assert!(!marker.exists(), "the kill reached the child");
    }

    fn deferred_command(name: &str, command: &str) -> Variable {
        Variable {
            name: name.to_string(),
            source: VarSource::SpawnCommand(ShellDecl::Platform),
            text: Template::extract(command),
            tier: Tier::Spawn,
            span: 0..0,
        }
    }

    /// The platform shell's spelling of "append one `x` to COUNTER,
    /// then print the counter's contents" — output that CHANGES with
    /// every run, which is what deferred means.
    fn append_then_read(counter: &std::path::Path) -> String {
        #[cfg(unix)]
        {
            format!("printf x >> {c}; cat {c}", c = counter.display())
        }
        // The parens are load-bearing: cmd strips the redirection but
        // keeps the space that preceded `&`, so a bare `echo x>> f`
        // appends "x " and the value under test grows a trailing
        // space — one the trim rule deliberately keeps.
        #[cfg(windows)]
        {
            format!("(echo x)>> \"{c}\" & type \"{c}\"", c = counter.display())
        }
    }

    fn runs(counter: &std::path::Path) -> usize {
        std::fs::read_to_string(counter)
            .map(|text| text.matches('x').count())
            .unwrap_or(0)
    }

    #[test]
    fn a_deferred_variable_derives_once_per_call() {
        // Three calls model three consuming spawns exactly — the
        // function is what a spawn calls, once.
        let dir = tempfile::tempdir().expect("tempdir");
        let counter = dir.path().join("runs");
        let block = VariableBlock::new(
            vec![deferred_command("head", &append_then_read(&counter))],
            vec![0],
        );
        let mut seen = Vec::new();
        for _ in 0..3 {
            let bindings = resolve_for_spawn(&block, &Bindings::new(), &Bindings::new(), &["head"])
                .expect("derives");
            seen.push(bindings.get("head").cloned().expect("a value"));
        }
        assert_eq!(runs(&counter), 3);
        assert_ne!(seen[0], seen[1]);
        assert_ne!(seen[1], seen[2]);
    }

    #[test]
    fn a_deferred_variable_runs_once_per_spawn_however_often_it_is_referenced() {
        // One CALL whose `needed` names it twice — the shape a command
        // referencing {{head}} in two strings produces. Without this
        // pin, reference count silently becomes subprocess count AND a
        // single command could observe two different values for one
        // name; the second half is the worse bug.
        let dir = tempfile::tempdir().expect("tempdir");
        let counter = dir.path().join("runs");
        let block = VariableBlock::new(
            vec![deferred_command("head", &append_then_read(&counter))],
            vec![0],
        );
        let bindings = resolve_for_spawn(
            &block,
            &Bindings::new(),
            &Bindings::new(),
            &["head", "head"],
        )
        .expect("derives");
        assert_eq!(runs(&counter), 1);
        let both = Template::extract("{{head}}={{head}}")
            .expand(&bindings)
            .expect("expands");
        let (left, right) = both.split_once('=').expect("two halves");
        assert_eq!(left, right, "both occurrences agree");
    }

    #[test]
    fn a_name_absent_from_needed_is_never_derived() {
        // "Per CONSUMING spawn": a spawn that does not reference the
        // deferred variable must not pay for it.
        let dir = tempfile::tempdir().expect("tempdir");
        let counter = dir.path().join("runs");
        let block = VariableBlock::new(
            vec![deferred_command("head", &append_then_read(&counter))],
            vec![0],
        );
        resolve_for_spawn(&block, &Bindings::new(), &Bindings::new(), &["head"]).expect("derives");
        resolve_for_spawn(&block, &Bindings::new(), &Bindings::new(), &[]).expect("derives");
        assert_eq!(
            runs(&counter),
            1,
            "the empty-needed call contributed nothing"
        );
    }

    #[test]
    fn a_failing_deferred_derivation_is_worded_for_the_panes_box() {
        let block = VariableBlock::new(vec![deferred_command("head", "exit 7")], vec![0]);
        let err = resolve_for_spawn(&block, &Bindings::new(), &Bindings::new(), &["head"])
            .expect_err("the derivation fails");
        let text = err.to_string();
        assert!(text.contains(r#"variable "head""#), "{text}");
        assert!(text.contains('7'), "{text}");
    }

    #[test]
    fn a_zero_length_read_under_defer_fails_rather_than_expanding_empty() {
        // The interrupted zero-length write: the writer truncated but
        // has not written yet. A silent empty expansion would send
        // `--revision ""` to a real command, so the NEGATIVE half is
        // the point.
        let dir = tempfile::tempdir().expect("tempdir");
        let handoff = dir.path().join("handoff");
        std::fs::write(&handoff, "").expect("truncated");
        #[cfg(unix)]
        let command = format!("cat {}", handoff.display());
        #[cfg(windows)]
        let command = format!("type \"{}\"", handoff.display());
        let block = VariableBlock::new(vec![deferred_command("rev", &command)], vec![0]);
        let err = resolve_for_spawn(&block, &Bindings::new(), &Bindings::new(), &["rev"])
            .expect_err("a zero-length read is a transient the board must announce");
        let text = err.to_string();
        assert!(text.contains(r#"variable "rev""#), "{text}");
        assert!(text.contains("printed nothing"), "{text}");

        // The next tick recovers on its own — two calls are two ticks.
        std::fs::write(&handoff, "rev-43").expect("the writer finished");
        let bindings = resolve_for_spawn(&block, &Bindings::new(), &Bindings::new(), &["rev"])
            .expect("recovers");
        assert_eq!(bindings.get("rev").map(String::as_str), Some("rev-43"));
    }

    #[test]
    fn a_read_does_not_move_a_paths_fingerprint() {
        // The deterministic core of the loop-detector claim: the
        // detector's evidence is mtime, and a read moves no mtime — so
        // a deferred `cat` of a watched file is credited with nothing
        // and cannot appear on a cycle. If fingerprinting ever starts
        // considering atime or size, THIS fails first and cheaply.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("watched");
        std::fs::write(&path, "content").expect("write");
        let paths = [path.clone()];
        let before = crate::core::trigger::stamps(&paths);
        let _ = std::fs::read_to_string(&path);
        assert_eq!(crate::core::trigger::stamps(&paths), before);
    }

    #[test]
    fn an_override_supplies_a_deferred_value_and_suppresses_its_command() {
        // `-v` applies at the spawn tier by exact name, exactly as at
        // load — the value is supplied, the command never reached. (The
        // other half — that the override does NOT change the variable's
        // tier, so it stays illegal at a load-time site — is the site
        // rule's to assert.)
        let dir = tempfile::tempdir().expect("tempdir");
        let counter = dir.path().join("runs");
        let block = VariableBlock::new(
            vec![deferred_command("head", &append_then_read(&counter))],
            vec![0],
        );
        let overrides = Bindings::from([("head".to_string(), "abc".to_string())]);
        for _ in 0..3 {
            let bindings = resolve_for_spawn(&block, &Bindings::new(), &overrides, &["head"])
                .expect("resolves");
            assert_eq!(bindings.get("head").map(String::as_str), Some("abc"));
        }
        assert_eq!(runs(&counter), 0, "the command was never reached");
    }

    #[test]
    fn a_raw_string_holding_a_deferred_reference_runs_nothing() {
        // The refs-not-text rule made observable: a raw template's
        // record says it references nothing, so `needed` is empty, so
        // nothing derives — and expansion returns the literal bytes.
        // An implementation that re-scanned the bytes would spawn a
        // subprocess per tick for a string that never interpolates.
        let dir = tempfile::tempdir().expect("tempdir");
        let counter = dir.path().join("runs");
        let block = VariableBlock::new(
            vec![deferred_command("head", &append_then_read(&counter))],
            vec![0],
        );
        let raw = Template::literal("--revision {{head}}");
        let needed: Vec<&str> = raw.refs().iter().map(String::as_str).collect();
        let bindings = resolve_for_spawn(&block, &Bindings::new(), &Bindings::new(), &needed)
            .expect("resolves");
        assert_eq!(runs(&counter), 0);
        assert_eq!(
            raw.expand(&bindings).expect("expands"),
            "--revision {{head}}"
        );
    }

    #[test]
    fn a_deferred_chain_and_a_derived_dialect_both_resolve() {
        // Two routes the resolver's walk must cover beyond the direct
        // case: a deferred variable referencing another deferred one
        // (each link declares defer explicitly), and a deferred
        // variable whose DIALECT is a template — the shell's references
        // join the dependency set, or a derived dialect never resolves.
        let dir = tempfile::tempdir().expect("tempdir");
        let counter = dir.path().join("runs");
        let base = deferred_command("base", &append_then_read(&counter));
        #[cfg(unix)]
        let echo_cmd = "echo chained-{{base}}";
        #[cfg(windows)]
        let echo_cmd = "echo chained-{{base}}";
        let chained = deferred_command("chained", echo_cmd);
        // A dialect that is itself a template, resolved from the LOAD
        // map: the platform shell's own name, so the spawn is portable.
        let mut derived_dialect = deferred_command("dialect-user", "echo hi");
        derived_dialect.source =
            VarSource::SpawnCommand(ShellDecl::Named(Template::extract("{{dialect}}")));
        let block = VariableBlock::new(vec![base, chained, derived_dialect], vec![0, 1, 2]);
        let load = Bindings::from([("dialect".to_string(), platform_shell())]);
        let bindings = resolve_for_spawn(
            &block,
            &load,
            &Bindings::new(),
            &["chained", "dialect-user"],
        )
        .expect("resolves");
        // The chain: `chained` needed `base`, which derived once.
        assert_eq!(runs(&counter), 1);
        assert_eq!(
            bindings.get("chained").map(String::as_str),
            Some("chained-x")
        );
        assert_eq!(bindings.get("dialect-user").map(String::as_str), Some("hi"));
    }

    #[test]
    fn a_nonzero_exit_names_the_variable_the_code_and_the_command() {
        let err = refusal(
            "store",
            "exit 128",
            &DerivationFailure::Exit {
                code: Some(128),
                stderr: "fatal: not a git repository\n".into(),
            },
        );
        let text = format!("{err:#}");
        assert!(text.contains(r#"variable "store""#), "{text}");
        assert!(text.contains("128"), "{text}");
        // The command, echoed back so the author can paste it.
        assert!(text.contains("exit 128"), "{text}");
        // The child's own diagnostic.
        assert!(text.contains("fatal: not a git repository"), "{text}");
        // The fallback the shell already has.
        assert!(text.contains("||"), "{text}");

        // A signalled child has no code — name the signal's absence
        // rather than inventing one.
        let err = refusal(
            "store",
            "exit 128",
            &DerivationFailure::Exit {
                code: None,
                stderr: String::new(),
            },
        );
        let text = format!("{err:#}");
        assert!(text.contains("signal"), "{text}");
    }

    #[test]
    fn empty_output_explains_the_masking_it_prevents() {
        let err = refusal("store", "true", &DerivationFailure::Empty);
        let text = format!("{err:#}");
        assert!(text.contains(r#"variable "store""#), "{text}");
        assert!(text.contains("printed nothing"), "{text}");
        // The same fallback spelling as the exit arm.
        assert!(text.contains("||"), "{text}");
        // The REASON, because this is the failure an author will most
        // want to argue with: a silently-empty value expands into a
        // plausible-looking wrong path.
        assert!(text.contains("plausible"), "carries the rationale: {text}");

        // Whitespace-only output takes this arm, not the success arm.
        assert!(matches!(
            classify_value(b"   \n".to_vec(), false),
            Err(DerivationFailure::Empty)
        ));
    }

    #[test]
    fn a_spawn_failure_names_the_shell_not_the_command() {
        let err = refusal(
            "store",
            "git rev-parse --git-common-dir",
            &DerivationFailure::Spawn {
                program: "definitely-not-a-shell".into(),
                source: std::io::Error::from_raw_os_error(2),
            },
        );
        let text = format!("{err:#}");
        assert!(text.contains(r#"variable "store""#), "{text}");
        // The program that failed to start…
        assert!(text.contains("definitely-not-a-shell"), "{text}");
        // …with the OS reason, never an OS-specific sentence pinned.
        assert!(text.contains("os error"), "{text}");
        // The negative half: a reader who sees the COMMAND presented as
        // the missing program goes looking for git when the shell is
        // what is missing.
        assert!(!text.contains("git rev-parse"), "{text}");
    }

    #[test]
    fn a_timeout_names_the_variable_and_the_bound() {
        let err = refusal(
            "store",
            "some-slow-thing",
            &DerivationFailure::Timeout(DERIVE_TIMEOUT),
        );
        let text = format!("{err:#}");
        assert!(text.contains(r#"variable "store""#), "{text}");
        // The bound, in the file's own duration spelling — never Debug.
        assert!(text.contains("5s"), "{text}");
        assert!(!text.contains("5.0s"), "{text}");
        // The WHY: a once-at-load command runs before the first frame,
        // so a hanging one hangs startup with no UI to explain it.
        assert!(text.contains("before the first frame"), "{text}");
        // Deferring a hang moves the stall onto the frame loop — worse,
        // and not obviously so. The fix must not be `defer`.
        assert!(!text.contains("defer"), "{text}");
    }

    #[test]
    fn a_dialect_that_expanded_to_nothing_names_the_variable_and_the_shell_reference() {
        let err = refusal(
            "store",
            "git rev-parse --git-common-dir",
            &DerivationFailure::EmptyShell(crate::core::registry::EmptyShellName {
                declared: "{{d}}".into(),
            }),
        );
        let text = format!("{err:#}");
        assert!(text.contains(r#"variable "store""#), "{text}");
        assert!(text.contains("shell"), "{text}");
        // The dialect AS WRITTEN, holes and all — the thing the author
        // can go edit, not the blank it produced.
        assert!(text.contains("{{d}}"), "{text}");
    }

    #[test]
    fn output_that_overflows_its_bound_is_a_loud_failure() {
        let err = refusal(
            "listing",
            "find /",
            &DerivationFailure::Truncated { cap: VALUE_CAP },
        );
        let text = format!("{err:#}");
        assert!(text.contains(r#"variable "listing""#), "{text}");
        // The message spells a narrowing fix.
        assert!(text.contains("head -1"), "{text}");
    }

    #[test]
    fn every_failure_variant_names_its_variable() {
        // The exhaustive pin: constructed one per variant, and the
        // `match` below is what turns "someone added an eighth variant
        // and forgot the rule" into a compile error rather than a gap.
        let all = [
            DerivationFailure::Expansion(crate::core::template::MissingVariable {
                name: "x".into(),
                offset: 0,
            }),
            DerivationFailure::EmptyShell(crate::core::registry::EmptyShellName {
                declared: "{{d}}".into(),
            }),
            DerivationFailure::Spawn {
                program: "sh".into(),
                source: std::io::Error::from_raw_os_error(2),
            },
            DerivationFailure::Exit {
                code: Some(1),
                stderr: String::new(),
            },
            DerivationFailure::Truncated { cap: 1 },
            DerivationFailure::Empty,
            DerivationFailure::Timeout(Duration::from_secs(5)),
        ];
        for failure in all {
            match &failure {
                DerivationFailure::Expansion(_)
                | DerivationFailure::EmptyShell(_)
                | DerivationFailure::Spawn { .. }
                | DerivationFailure::Exit { .. }
                | DerivationFailure::Truncated { .. }
                | DerivationFailure::Empty
                | DerivationFailure::Timeout(_) => {}
            }
            let text = format!("{:#}", refusal("the-name", "cmd", &failure));
            assert!(
                text.contains(r#"variable "the-name""#),
                "{failure:?} → {text}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_command_that_floods_both_pipes_still_finishes() {
        // Far more than a pipe buffer to stderr while stdout carries
        // the value: a serial drain deadlocks on exactly this child.
        let line = "x".repeat(100);
        let command = format!(
            "i=0; while [ $i -lt 3000 ]; do echo {line} >&2; i=$((i+1)); done; printf done"
        );
        let request = Derivation {
            name: "flood",
            command: &command,
            shell: &ShellMode::Platform,
        };
        assert_eq!(derive(&request).expect("finishes"), "done");
        // And a stdout flood past the byte ceiling FINISHES loudly as
        // Truncated — never a deadlock, never Empty.
        let command = format!("i=0; while [ $i -lt 3000 ]; do echo {line}; i=$((i+1)); done");
        let request = Derivation {
            name: "flood-out",
            command: &command,
            shell: &ShellMode::Platform,
        };
        assert!(matches!(
            derive(&request),
            Err(DerivationFailure::Truncated { .. })
        ));
    }
}
