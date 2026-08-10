//! `rat dashboard`: N declared panes, one flicker-free frame. Thin by
//! construction — the declaration file becomes a [`Registry`], the
//! flags become a `SessionArgs`, and the watch engine does the rest.

use anyhow::{Context, bail};

use crate::cli::{CheckArgs, DashboardAction, DashboardArgs, InitArgs};
use crate::color::ColorProfile;
use crate::commands::watch::{SessionArgs, run_registry};
use crate::core::dashboard_file::{
    Board, DashboardFile, KeyBinding, KeyBindingDecl, SiteAudit, UncheckedSite, finish_load,
    read_and_parse,
};
use crate::core::registry::Registry;
use crate::core::template::{Bindings, is_reference_name};
use crate::exit::AppResult;
use crate::theme::Palette;

pub fn run(args: DashboardArgs, profile: ColorProfile, palette: Palette) -> AppResult {
    // Dispatched first: the subcommand needs no terminal, no tab
    // title, and no Registry-backed session.
    match args.action {
        Some(DashboardAction::Check(check)) => return check_board(check, profile),
        Some(DashboardAction::Init(init_args)) => return init(init_args),
        None => {}
    }
    // Honest rather than defensive: `required = true` plus
    // `subcommand_negates_reqs` makes `None` unreachable once no
    // subcommand was given, and a silent default would hide a
    // clap-config regression the bare-`rat dashboard` test exists to
    // catch.
    let file = args
        .file
        .clone()
        .expect("clap requires FILE without a subcommand");
    // A load error prints before any UI exists, so the profile is the
    // one color authority it gets: anything above Ascii earns the
    // colored snippet theme.
    let overrides = parse_overrides(&args.variable)?;
    let board = validated(&file, profile != ColorProfile::Ascii, &overrides)?;
    let registry = board.registry;
    let variables = board.variables;
    let bindings = board.bindings;
    let once_timeout = args
        .once_timeout
        .as_deref()
        .map(crate::core::duration::parse_interval)
        .transpose()
        .map_err(|err| anyhow::anyhow!("--once-timeout: {err:#}"))?;
    let session = SessionArgs {
        once: args.once,
        // The declaration file's stem is the tab's fallback identity;
        // the declared title (static or pane-sourced) outranks it at
        // emit time.
        tab_title: Some(
            file.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| "dashboard".to_string()),
        ),
        once_timeout,
        clear: args.clear,
        fullscreen: args.fullscreen,
        mouse: args.mouse,
        no_hide_cursor: args.no_hide_cursor,
        no_sync: args.no_sync,
        // Declared geometry: a wrapped line would add rows the composed
        // frame's run-constant height did not budget for.
        wrap: false,
        max_height: args.max_height,
        snapshot_dir: args.snapshot_dir.clone(),
        snapshot_ansi: args.snapshot_ansi,
        live_tail: dashboard_suffix(args.once, registry.len()),
        help_heading: "rat dashboard — keys",
        help_extra: pane_help(&registry, &bindings),
        // Boxes are allocated from the terminal width, so a resize
        // reflows and every child is respawned under the new geometry.
        resize_respawn: true,
        // Append is watch-only today: a dashboard's composed panes
        // don't linearize, and this arm's resize/reflow machinery would
        // need its own treatment first.
        //
        // It is also the reason the line cursor's mark reaches nobody
        // who cannot see it: the mark is visual only, and a linear mode
        // for boards is the missing piece, not a better mark.
        append: false,
        variables,
        bindings,
    };
    run_registry(registry, session, profile, palette)
}

/// Read, parse, refuse a claimed key, then finish loading — the path
/// every `Board`-producing route in this command takes.
///
/// **What this does and does not guarantee.** It removes the ordering
/// question from any route that wants a loaded board: `run` cannot
/// obtain one without passing the refusal. It does NOT make the
/// refusal unskippable in general — `finish_load` is reachable on its
/// own, and `check` deliberately bypasses this function because it
/// must never build a `Registry`. A route that does not want a
/// `Board` has to invoke the refusal itself and carry a test for it.
///
/// The order is the point. The refusal sits between a parsed board and
/// a loaded one — it needs the declarations, and a board that cannot
/// have its key should not pay to derive a single variable. It lives
/// here rather than in core because it reads the loop's key table.
pub(crate) fn validated(
    path: &std::path::Path,
    colored: bool,
    overrides: &Bindings,
) -> anyhow::Result<Board> {
    let (text, file) = read_and_parse(path, colored, overrides)?;
    refuse_claimed_bindings(&file.bindings, file.takes_a_cursor())
        .with_context(|| format!("in {}", path.display()))?;
    finish_load(path, &text, file, overrides)
}

/// Built-ins always win, and they win loudly. A board author who
/// writes `key "j"` gets an error naming what `j` already does — the
/// alternative is a binding that quietly never fires, which is the
/// worst outcome this grammar can produce.
///
/// `takes_a_cursor` is the board's own answer to whether any pane asked
/// for a line cursor, and it is the one input that can un-claim a key:
/// the cursor toggle has nothing to act on where nothing asked for it,
/// so there the letter is the board's. It comes from the declarations
/// rather than a registry, which is what keeps this check ahead of the
/// load step.
///
/// Takes the DECLARED form, and runs before the load step expands
/// anything: it needs only the key and the spelling, both of which
/// survive the walk, and a board whose key is not its to bind should
/// not pay for expansion first.
///
/// Called from every entry point that validates a board. It is here,
/// and not in `into_registry`, because its two inputs sit on opposite
/// sides of the layering: the loop's table and the board's
/// declaration.
pub(crate) fn refuse_claimed_bindings(
    bindings: &[KeyBindingDecl],
    takes_a_cursor: bool,
) -> anyhow::Result<()> {
    for binding in bindings {
        if let Some(does) = crate::commands::watch::builtin_key(binding.key, takes_a_cursor) {
            // The author's own bytes, not a re-rendering: the error
            // quotes the line they wrote.
            let spelling = &binding.spelling;
            bail!(
                "key {spelling:?}: `{spelling}` is one of rat's own keys — it {does}. \
                 Press `?` on any board for the full list, then pick a key it does not name"
            );
        }
    }
    Ok(())
}

/// The run-constant tail of every live row — the same rule as watch's
/// `live_suffix`: nothing here may count, or the repaint gate is
/// defeated and a parked dashboard stops being byte-silent. The source
/// count is the one fact worth the width.
fn dashboard_suffix(once: bool, sources: usize) -> String {
    if once {
        return String::new();
    }
    match sources {
        1 => " · 1 source · ? help".to_string(),
        n => format!(" · {n} sources · ? help"),
    }
}

/// The dashboard's slice of the `?` reference: one line per pane with
/// its cadence, any trigger specs indented beneath — the same shape as
/// watch's trigger section.
fn pane_help(registry: &Registry, bindings: &[KeyBinding]) -> Vec<String> {
    let mut lines = vec![String::new(), "  panes:".to_string()];
    for id in registry.ids() {
        let spec = registry.spec(id);
        lines.push(format!(
            "    {}  {}",
            spec.id,
            crate::commands::watch::cadence_label(spec)
        ));
        for trigger in &spec.triggers {
            lines.push(format!("      {trigger}"));
        }
    }
    // The gestures themselves are unconditional, unlike the sections
    // below: every dashboard registry is Composition::Panes, so they
    // always apply. The cursor's rows inside the block are not — the
    // key is the board's own where no pane asked for one.
    lines.extend(pane_gesture_help(
        registry
            .ids()
            .any(|id| registry.pane(id).is_some_and(|pane| pane.selectable)),
    ));
    lines.extend(binding_help(bindings));
    if registry.ids().any(|id| registry.spec(id).live) {
        lines.extend(LIVE_HELP.iter().map(|l| (*l).to_string()));
    }
    if registry
        .ids()
        .any(|id| !registry.spec(id).triggers.is_empty())
    {
        lines.extend(LOOPING_HELP.iter().map(|l| (*l).to_string()));
    }
    // The diagnostics channel: load-time facts worth telling but not
    // worth failing over. Absent diagnostics add NOTHING — the plain
    // help stays byte-identical.
    if !registry.diagnostics().is_empty() {
        lines.push(String::new());
        lines.push("  diagnostics:".to_string());
        for diagnostic in registry.diagnostics() {
            // The pager has no wrapping engine; the key table's width
            // is the budget every help line keeps.
            lines.push(crate::core::measure::truncate_display(
                &format!("    {diagnostic}"),
                74,
                crate::core::measure::ELLIPSIS,
            ));
        }
    }
    lines
}

/// The per-pane gestures, gated to a live frame. "live" here is the
/// FRAME state (not paused — a scrolled live frame counts, and the
/// frame window follows the focus) — a different word from the `live`
/// pane label the next section defines, and the heading says so
/// because the two share a spelling and nothing else.
///
/// Wrapped by hand, like every section here: `?` pages plain grouped
/// text through the pager, and nothing may exceed the key table's width.
/// The board's own keys, from the board's own declaration — the only
/// place a binding is advertised, which is why `description` is
/// required rather than optional: a key `?` cannot name is a trap.
///
/// EMPTY IN, EMPTY OUT: a board that declares no binding must produce
/// byte-identical help to the one it produced before bindings existed,
/// and `the_help_is_unchanged_when_no_pane_has_a_trigger` is the
/// witness.
fn binding_help(bindings: &[KeyBinding]) -> Vec<String> {
    /// The key column's width, chosen so descriptions start at cell 21
    /// — the column every shipped table already uses: `help_lines`
    /// writes `"  q                  quit"` (2 + 1 + 18) and the pane
    /// gestures write 4 + 11 + 6. Change it only with them.
    const KEY_COLUMN: usize = 17;
    if bindings.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![
        String::new(),
        "  key actions (this board's own keys):".to_string(),
    ];
    for binding in bindings {
        // Annotate what changes what the keypress COSTS the reader: a
        // confirm interrupts, so the reader must be ready to answer. A
        // `when` gets no note — its decline names the key at the
        // moment it happens, and on a real console nearly every
        // binding carries one, so a note would discriminate nothing.
        // Built as a joined list so a later note is an append, not a
        // reshape.
        let mut notes: Vec<&str> = Vec::new();
        if binding.confirm.is_some() {
            notes.push("asks before running");
        }
        let body = if notes.is_empty() {
            binding.description.clone()
        } else {
            format!("{} ({})", binding.description, notes.join(", "))
        };
        // `{:<17}` pads by chars, and that is correct for exactly one
        // reason: a key spelling is ASCII (the spelling grammar is the
        // intersection of the two input wires, and the unix tap drops
        // non-ASCII), so byte == char == cell. If the spellable set
        // ever widens past ASCII this misaligns every description —
        // the dependency is stated here so nobody "fixes" it early.
        // The description side gets no such guarantee, which is why
        // the COMPOSED line is bounded in display cells: the pager has
        // no wrapping engine, and the cut lands in the description,
        // never in the key.
        lines.push(crate::core::measure::truncate_display(
            &format!("    {:<KEY_COLUMN$}{body}", binding.spelling),
            74,
            crate::core::measure::ELLIPSIS,
        ));
    }
    lines.extend(BINDING_HELP_TAIL.iter().map(|l| (*l).to_string()));
    lines
}

/// The two facts a reader cannot guess from a list of keys: an action
/// is launch-and-forget, so the panes turn over on their own cadence
/// rather than on the keypress; and an action can decline before it
/// runs.
///
/// Wrapped by hand, like every section here: `?` pages plain grouped
/// text through the pager, and nothing may exceed the key table's
/// width.
const BINDING_HELP_TAIL: &[&str] = &[
    "",
    "    A key action runs one command and forgets it: the board learns",
    "    what changed the way it always does, when its panes next run —",
    "    on their intervals and their triggers, not the instant the key",
    "    is pressed. An action can also decline before it runs; the",
    "    status row names the key that declined.",
];

/// The gestures every board answers, up to the `Esc` row — which is
/// where the two shapes diverge, because the rung Esc peels first
/// exists only on a board that can hold a cursor.
const PANE_GESTURE_HELP_HEAD: &[&str] = &[
    "",
    "  pane gestures (while the frame is live — not the `live` label):",
    "    Tab, BackTab     cycle focus between panes — a zoom rides along",
    "    Alt-h/j/k/l      move focus directionally",
    "    Alt-1..9         jump straight to a numbered focusable pane",
];

/// The cursor's own row, between the focus gestures and `Esc` — where
/// a reader looks for a key, not appended in a clump at the end.
const CURSOR_GESTURE_ROW: &str =
    "    s                put a line cursor in the focused pane, or drop it";

/// `Esc`, in its two forms. The ladder it peels is the board's, and
/// naming a rung that cannot exist would be a reference that lies.
const ESC_ROW_WITH_CURSOR: &str =
    "    Esc              peel one layer: cursor, zoom, focus, frame scroll";
const ESC_ROW: &str = "    Esc              peel one layer: zoom, focus, frame scroll";

const PANE_GESTURE_HELP_TAIL: &[&str] = &[
    "    Enter            zoom the focused pane; zoomed, page its body",
    "    z                zoom the focused pane to the full frame",
    "    Space            collapse the focused pane to its title row",
    "    With a pane focused, the scroll keys and the wheel drive that",
    "    pane's own window instead of the whole frame, and focusing a",
    "    pane below the fold scrolls the frame to it. h/l shift",
    "    nothing here: pane content is clipped to its box. While a",
    "    focus is held, every focusable title counts itself in layout",
    "    order; Alt-1..9 jumps to the first nine (from rest, too),",
    "    and Tab reaches the rest of a larger board.",
];

/// What the cursor changes about keys the block above already
/// documents. It follows the prose it qualifies.
const CURSOR_GESTURE_HELP: &[&str] = &[
    "    With a cursor up, the scroll keys move the cursor instead and",
    "    the pane's window follows it; a key action then reads the",
    "    marked line as RAT_SELECTION. The cursor holds its line when",
    "    the pane re-runs, and a paused frame ignores all of this.",
];

/// The pane-gesture block for one board. A cursor is opt-in, so on a
/// board where no pane asked for one the key does nothing and belongs
/// to that board's own bindings — a reference naming it there would be
/// wrong, not merely noisy.
fn pane_gesture_help(cursor: bool) -> Vec<String> {
    let mut lines: Vec<String> = PANE_GESTURE_HELP_HEAD
        .iter()
        .map(|l| (*l).to_string())
        .collect();
    if cursor {
        lines.push(CURSOR_GESTURE_ROW.to_string());
    }
    lines.push(if cursor { ESC_ROW_WITH_CURSOR } else { ESC_ROW }.to_string());
    lines.extend(PANE_GESTURE_HELP_TAIL.iter().map(|l| (*l).to_string()));
    if cursor {
        lines.extend(CURSOR_GESTURE_HELP.iter().map(|l| (*l).to_string()));
    }
    lines
}

/// What the `live` label means, and what `interval` means under it.
///
/// **Static, like the looping section below**: `pane_help` runs once at
/// startup, so this states the contract rather than live state.
///
/// The `interval` sentence is the load-bearing one and is not guessable:
/// on a live pane the interval does nothing while the child runs, and
/// becomes the delay before a replacement spawns if it exits. Written as
/// what the knob DOES today, not as a promise that it is the only knob
/// there will ever be — a dedicated backoff can join it without making
/// this section a lie.
///
/// Wrapped by hand: `?` pages plain grouped text through the pager, so
/// there is no wrapping engine to lean on, and nothing here may exceed
/// the width the key table already sets.
const LIVE_HELP: &[&str] = &[
    "",
    "  live panes:",
    "    A pane marked `live` runs one long-lived command, spawned once",
    "    and painted as it prints — it is not on a cadence. Its",
    "    `interval` is how soon a replacement spawns if the child exits,",
    "    not how often it runs. `interval \"never\"` means no replacement:",
    "    the pane keeps its exit badge. A `trigger` on a live pane",
    "    restarts the child: the running one is killed and a replacement",
    "    spawned in its place.",
];

/// What `· looping` means, and what to do about it.
///
/// **Static, and it does not name the looping panes.** `pane_help` is
/// called once at startup and its result is stored in the session, so
/// `?` cannot report live state without making help dynamic — a bigger
/// change than this earns. The live naming is the notice row's job; it
/// already names the panes and the paths. This is a deliberate
/// departure from the sketch, which showed `?` listing the specific
/// panes, and it is recorded here so it does not read as an oversight.
///
/// **The closing paragraph is about what the badge's ABSENCE does not
/// mean**, and it is here because the detector can now decline to answer.
/// A window abstains when a candidate's reader evidence is all ambiguous,
/// and condition 3 has always abstained on lost evidence or a busy
/// dashboard — none of which reach a surface. So a silent dashboard and a
/// clean one look identical from outside, which is a quieter form of the
/// confident-negative defect the interval model removed.
///
/// Deliberately STATIC, and deliberately not a live "cannot decide" badge.
/// Nobody has measured how often abstention actually fires, and
/// `Verdict::abstained` carries no user-facing cause to put in a notice —
/// so a dynamic surface would be guessing at both its frequency and its
/// wording. Stating the limitation once, where the badge is explained,
/// costs nothing and is true today.
///
/// The mtime sentence below is the load-bearing one and is not guessable:
/// `fingerprint` is mtime-only, so a command that rewrites a file with
/// identical bytes still fires the trigger. Guarding the *change*
/// rather than the *write* is the mistake that produces exactly this
/// badge, and it is a real reported confusion elsewhere, not a
/// hypothetical one.
///
/// Wrapped by hand: `?` pages plain grouped text through the pager, so
/// there is no wrapping engine to lean on, and nothing here may exceed
/// the width the key table already sets.
const LOOPING_HELP: &[&str] = &[
    "",
    "  looping panes:",
    "    A pane marked `· looping` is still running — nothing has been",
    "    stopped. rat cannot see who writes a file, only that a watched",
    "    path changes while the dashboard is busy and never while it is",
    "    idle, which is what a pane whose own command touches another",
    "    pane's trigger looks like.",
    "",
    "    The fix is in the declaration: give the command a guard so it",
    "    writes only when the content changed, or point the trigger at a",
    "    path no pane writes. A trigger fires on mtime, not on content,",
    "    so writing identical bytes still fires — the guard has to skip",
    "    the write, not just the change.",
    "",
    "    The absence of the badge is weaker than its presence. rat stays",
    "    silent whenever it cannot tell: when a write cannot be placed",
    "    against the commands that were running, when a reader's evidence",
    "    was lost, or when the dashboard was too busy to judge. No badge",
    "    means no loop was proved, not that there is none.",
];

/// `-v name=value` into the map a resolution runs against. Split at
/// the FIRST `=` so a value may contain as many as it likes
/// (`-v flags=--since=yesterday` is a real board's real value), and
/// the name obeys the same grammar a `{{name}}` does — a name that
/// could never be referenced cannot be set.
///
/// `rat dashboard check` reuses this: the same flags must reach the
/// same verdict there.
/// Every shipped example, reachable from an installed binary. The
/// name is the file's stem, so the README's link and the template
/// name are the same string — the templates ARE the shipped examples,
/// embedded at compile time because no `examples/` directory exists
/// after `cargo install`, and self-contained because they run from
/// wherever `init` lands them.
struct BoardTemplate {
    name: &'static str,
    summary: &'static str,
    body: &'static str,
}

const TEMPLATES: &[BoardTemplate] = &[
    BoardTemplate {
        name: "panes",
        summary: "three commands, three cadences, one frame (the default)",
        body: include_str!("../../examples/panes.kdl"),
    },
    // NOT registered: examples/panes-nested.kdl. Its dashboard-in-a-
    // dashboard pane needs a second board file on disk BY CONSTRUCTION
    // — that is the feature it demonstrates — so it cannot stand alone
    // outside a clone. It stays an example; the registry rule is
    // "every shipped example that is self-contained", applied.
    BoardTemplate {
        name: "script",
        summary: "multi-line script bodies, with and without a #! line",
        body: include_str!("../../examples/script.kdl"),
    },
    BoardTemplate {
        name: "follow",
        summary: "a live log follower beside a batch pane",
        body: include_str!("../../examples/follow.kdl"),
    },
    BoardTemplate {
        name: "tail",
        summary: "the follower made self-feeding — needs no second terminal",
        body: include_str!("../../examples/tail.kdl"),
    },
    BoardTemplate {
        name: "tail-windows",
        summary: "the same board for cmd.exe",
        body: include_str!("../../examples/tail-windows.kdl"),
    },
    BoardTemplate {
        name: "variables",
        summary: "the three evaluation forms, and a raw string that stays literal",
        body: include_str!("../../examples/variables.kdl"),
    },
    BoardTemplate {
        name: "review",
        summary: "a review console: derived paths, the handoff file, event triggers",
        body: include_str!("../../examples/review.kdl"),
    },
    BoardTemplate {
        name: "keys",
        summary: "a board that acts: bindings with a guard, a confirm, and a pager",
        body: include_str!("../../examples/keys.kdl"),
    },
    BoardTemplate {
        name: "keys-windows",
        summary: "the same board for cmd.exe",
        body: include_str!("../../examples/keys-windows.kdl"),
    },
];

/// `panes` because it is the README's own first example: `init` with
/// no flags produces the board a reader has already seen.
const DEFAULT_TEMPLATE: &str = "panes";

/// `init` takes no color profile and no palette, deliberately: the
/// output is a FILE, and a function that cannot see the palette
/// cannot accidentally style it.
fn init(args: InitArgs) -> AppResult {
    if args.list {
        for template in TEMPLATES {
            println!("{:<14} {}", template.name, template.summary);
        }
        return Ok(());
    }
    let name = args.template.as_deref().unwrap_or(DEFAULT_TEMPLATE);
    let template = TEMPLATES.iter().find(|t| t.name == name).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown template {name:?} — the templates are {}",
            TEMPLATES
                .iter()
                .map(|t| t.name)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    match &args.output {
        None => print!("{}", template.body),
        Some(path) => write_new(path, template.body)?,
    }
    Ok(())
}

/// Write the template, refusing to overwrite: `create_new` checks and
/// claims in one syscall. No retry suffix, deliberately — the user
/// NAMED this file, and silently writing a `-2` sibling would hide the
/// collision behind a path they did not ask for. Overwriting is
/// `rat dashboard init > board.kdl`, which already means what it says.
fn write_new(path: &std::path::Path, body: &str) -> anyhow::Result<()> {
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => file
            .write_all(body.as_bytes())
            .with_context(|| format!("writing {}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!(
                "{} already exists — pick another path, or redirect stdout",
                path.display()
            )
        }
        Err(err) => Err(anyhow::Error::new(err).context(format!("writing {}", path.display()))),
    }
}

/// One report row: an `UncheckedSite` from the audit, enriched with
/// the commands actually responsible. Private to the command layer —
/// `roots` needs the variable graph, and root-cause tracing is report
/// logic, which is why the core DTO deliberately stops at `blockers`.
struct SkipRow {
    site: UncheckedSite,
    roots: Vec<String>,
}

/// Validate a declaration file and report, without running anything it
/// declares: the shared load prefix, then the partial, non-executing
/// resolver in place of the real one.
fn check_board(args: CheckArgs, profile: ColorProfile) -> AppResult {
    use crate::core::variables::{opaque_roots, resolve_partial};
    let colored = profile != ColorProfile::Ascii;
    let overrides = parse_overrides(&args.variable)?;
    // The shared prefix — read + parse + `-v` validation — with BOTH
    // context strings attached exactly once. `validated()` composes
    // this same function; inlining the steps here is how the
    // byte-identity with a run's stderr would quietly stop being true.
    let (_text, file) = read_and_parse(&args.file, colored, &overrides)?;
    // The claimed-key refusal, ahead of the branch — `check` cannot go
    // through `validated()` because that ends in a `Registry` this
    // command must never build. A future board-validating route
    // carries the same two obligations: invoke the refusal, and add an
    // arm to the two-route integration test.
    refuse_claimed_bindings(&file.bindings, file.takes_a_cursor())
        .with_context(|| format!("in {}", args.file.display()))?;
    // NOT resolve_variables: no command runs. The partial resolver
    // walks the same graph and returns Known for constants, -v
    // overrides, and chains of those — so a malformed CONSTANT still
    // reaches its token parser and still refuses — and Opaque for
    // anything a command would have to produce. Infallible: every
    // refusal already fired during the parse inside the prefix.
    let partial = resolve_partial(&file.variables, &overrides);
    // A malformed KNOWN value refuses here, exactly as a run would;
    // only Skipped becomes a row.
    let audit = file
        .audit_sites(&partial)
        .with_context(|| format!("in {}", args.file.display()))?;
    let rows: Vec<SkipRow> = audit
        .unchecked
        .iter()
        .map(|site| SkipRow {
            roots: {
                let mut roots: Vec<String> = Vec::new();
                for blocker in &site.blockers {
                    for root in opaque_roots(&file.variables, &partial, blocker) {
                        if !roots.contains(&root) {
                            roots.push(root);
                        }
                    }
                }
                roots
            },
            site: UncheckedSite {
                origin: site.origin.clone(),
                key: site.key,
                blockers: site.blockers.clone(),
            },
        })
        .collect();
    let _ = SiteAudit::default();
    print!(
        "{}",
        render_report(&file, &partial, &overrides, &rows, &args.file)
    );
    Ok(())
}

/// The names a board's templates reference anywhere — panes, defaults,
/// the titles, and other variables. What is declared minus this is the
/// unused-variable notice.
fn referenced_names(file: &DashboardFile) -> std::collections::BTreeSet<String> {
    let mut used = std::collections::BTreeSet::new();
    let mut take = |template: &crate::core::template::Template| {
        for name in template.refs() {
            used.insert(name.clone());
        }
    };
    for decl in file.panes.iter().chain(std::iter::once(&file.defaults)) {
        for template in decl.command.iter().flatten() {
            take(template);
        }
        for template in decl.trigger.iter().flatten() {
            take(template);
        }
        for template in [
            &decl.script,
            &decl.interval,
            &decl.trigger_debounce,
            &decl.width,
            &decl.overflow,
            &decl.border,
            &decl.padding,
            &decl.title,
        ]
        .into_iter()
        .flatten()
        {
            take(template);
        }
        if let Some(crate::core::registry::ShellDecl::Named(name)) = &decl.shell {
            take(name);
        }
    }
    if let Some(title) = &file.title
        && let Some(text) = &title.text
    {
        take(text);
    }
    for name in file.variables.declared_list().split(", ") {
        if let Some(variable) = file.variables.get(name) {
            for referent in variable.refs() {
                used.insert(referent.to_string());
            }
        }
    }
    used
}

/// The whole stdout report, as a pure function so its shape is
/// unit-testable without a process per assertion. Plain text — no
/// boxes, no frames; `check` never touches the terminal.
fn render_report(
    file: &DashboardFile,
    partial: &crate::core::variables::Partial,
    overrides: &Bindings,
    rows: &[SkipRow],
    path: &std::path::Path,
) -> String {
    use std::fmt::Write;

    use crate::core::variables::{Resolved, VarSource};
    let mut out = String::new();
    let panes = file.panes.len();
    let variables = partial.len();
    let pane_word = if panes == 1 { "pane" } else { "panes" };
    let variable_word = if variables == 1 {
        "variable"
    } else {
        "variables"
    };
    let _ = writeln!(
        out,
        "{}: {panes} {pane_word}, {variables} {variable_word} — ok",
        path.display()
    );
    if variables == 0 {
        return out;
    }
    let _ = writeln!(out, "\nvariables");
    // The partial map holds every declared name (deferred ones
    // included, as Opaque), and a BTreeMap iterates sorted — the same
    // scannable order the declared-set breadcrumb uses.
    let names: Vec<&str> = partial.keys().map(String::as_str).collect();
    let width = names.iter().map(|name| name.len()).max().unwrap_or(0);
    for name in &names {
        let variable = file.variables.get(name).expect("declared");
        // The EFFECTIVE tier, not the declared form: a constant that
        // references a once-at-load command IS once-at-load, and the
        // promotion is the thing the reader most needs to see. A
        // `-v`-overridden DEFERRED name still shows `deferred`: the
        // tier is what the site rule reads, and an override never
        // changes it.
        let tier = match &variable.source {
            VarSource::SpawnCommand(_) => "deferred",
            VarSource::LoadCommand(_) => "once at load",
            VarSource::Constant => match partial.get(*name) {
                Some(Resolved::Opaque) if !overrides.contains_key(*name) => "once at load",
                _ => "constant",
            },
        };
        let status = match partial.get(*name) {
            Some(Resolved::Known(_)) => "checked",
            _ => "opaque — derived by a command",
        };
        let flag = if overrides.contains_key(*name) {
            "  (-v)"
        } else {
            ""
        };
        let _ = writeln!(out, "  {name:width$}  {tier:13} {status}{flag}");
    }
    let used = referenced_names(file);
    let unused: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| !used.contains(*name))
        .collect();
    if !unused.is_empty() || !rows.is_empty() {
        let _ = writeln!(out, "\nnotices");
        for name in unused {
            let _ = writeln!(out, "  {name} is declared and never referenced");
        }
        if !rows.is_empty() {
            // One row per DECLARATION: a defaults value inherited by N
            // panes is one row, because there is one edit.
            let mut seen: Vec<String> = Vec::new();
            let mut lines: Vec<String> = Vec::new();
            for row in rows {
                let origin = row.site.origin.to_string();
                let cause = if row.site.blockers == row.roots {
                    format!("{} is derived by a command", row.roots.join(", "))
                } else {
                    format!(
                        "{} → {}, derived by a command",
                        row.site.blockers.join(", "),
                        row.roots.join(", ")
                    )
                };
                let line = format!("    {origin} {key} — {cause}", key = row.site.key);
                if !seen.contains(&line) {
                    seen.push(line.clone());
                    lines.push(line);
                }
            }
            let (plural, verb) = if lines.len() == 1 {
                ("", "was")
            } else {
                ("s", "were")
            };
            let _ = writeln!(
                out,
                "  {} load-time site{plural} {verb} not checked, because their values derive at run time:",
                lines.len(),
            );
            for line in lines {
                let _ = writeln!(out, "{line}");
            }
        }
    }
    out
}

pub(crate) fn parse_overrides(args: &[String]) -> anyhow::Result<Bindings> {
    let mut out = Bindings::new();
    for arg in args {
        let Some((name, value)) = arg.split_once('=') else {
            bail!("-v takes `name=value` — write `-v plan=/path/to/plan`, not `-v {arg}`");
        };
        if !is_reference_name(name) {
            bail!(
                "-v {arg:?}: a variable's name starts with a letter or `_` and continues \
                 with letters, digits, `_` or `-` — the same spelling `{{{{name}}}}` uses"
            );
        }
        if out.insert(name.to_string(), value.to_string()).is_some() {
            bail!("-v {name} is given twice — give each variable one value");
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::core::box_model::{BorderPreset, Sides};
    use crate::core::registry::{
        LayoutNode, Overflow, PaneBox, PaneWidth, ShellMode, SourceId, SourceProgram, SourceSpec,
    };
    use crate::core::trigger::TriggerSpec;

    /// The ordinary board: two panes, neither of which asked for a
    /// line cursor. That is the shape of nearly every board, and the
    /// help block it produces is the one worth pinning byte for byte.
    fn registry(triggers: bool) -> Registry {
        registry_of(triggers, false)
    }

    /// The same board with a pane that asked for a cursor, for the arm
    /// that pins what the reference grows when one does.
    fn registry_with_a_cursor() -> Registry {
        registry_of(false, true)
    }

    fn registry_of(triggers: bool, selectable: bool) -> Registry {
        let spec = |id: &str, path: &str| SourceSpec {
            id: id.to_string(),
            program: SourceProgram::Argv(vec!["true".into()]),
            shell: ShellMode::Direct,
            interval: (!triggers).then(|| Duration::from_secs(5)),
            triggers: if triggers {
                vec![TriggerSpec::File(std::path::PathBuf::from(path))]
            } else {
                Vec::new()
            },
            debounce: Duration::from_millis(250),
            live: false,
        };
        let pane = || PaneBox {
            height: 5,
            width: PaneWidth::Weight(1),
            overflow: Overflow::KeepTop,
            border: BorderPreset::Rounded,
            padding: Sides::default(),
            title: None,
            chrome: true,
            focusable: true,
            selectable,
        };
        Registry::panes(
            vec![spec("a", "./sa"), spec("b", "./sb")],
            vec![pane(), pane()],
            LayoutNode::Row(vec![
                LayoutNode::Pane(SourceId(0)),
                LayoutNode::Pane(SourceId(1)),
            ]),
            1,
            0,
        )
        .expect("a valid two-pane registry")
    }

    /// One live follower beside one batch pane — the mixed dashboard
    /// the help must serve. The live pane keeps its tail, as load
    /// would have enforced.
    fn live_registry() -> Registry {
        let batch = SourceSpec {
            id: "a".to_string(),
            program: SourceProgram::Argv(vec!["true".into()]),
            shell: ShellMode::Direct,
            interval: Some(Duration::from_secs(5)),
            triggers: Vec::new(),
            debounce: Duration::from_millis(250),
            live: false,
        };
        let follow = SourceSpec {
            id: "follow".to_string(),
            program: SourceProgram::Argv(vec!["true".into()]),
            shell: ShellMode::Direct,
            interval: Some(Duration::from_secs(2)),
            triggers: Vec::new(),
            debounce: Duration::from_millis(250),
            live: true,
        };
        let pane = |overflow| PaneBox {
            height: 5,
            width: PaneWidth::Weight(1),
            overflow,
            border: BorderPreset::Rounded,
            padding: Sides::default(),
            title: None,
            chrome: true,
            focusable: true,
            selectable: true,
        };
        Registry::panes(
            vec![batch, follow],
            vec![pane(Overflow::KeepTop), pane(Overflow::KeepBottom)],
            LayoutNode::Row(vec![
                LayoutNode::Pane(SourceId(0)),
                LayoutNode::Pane(SourceId(1)),
            ]),
            1,
            0,
        )
        .expect("a valid two-pane registry")
    }

    #[test]
    fn the_help_explains_what_interval_means_on_a_live_pane() {
        // The owner contract, stated where a chrome row has no room:
        // `interval` on a live pane is the respawn delay. Not a
        // guessable fact — nothing on the pane says it. De-wrapped
        // before matching, for the same reason as the looping test.
        let lines = pane_help(&live_registry(), &[]);
        assert!(
            lines.contains(&"    follow  live".to_string()),
            "the pane list must carry the live label: {lines:?}"
        );
        let text = lines.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ");
        assert!(text.contains("live panes:"), "got {text}");
        assert!(text.contains("how soon a replacement spawns"), "got {text}");
        assert!(text.contains("not how often it runs"), "got {text}");
        // And the opt-out, which is the same contract's other half:
        // `interval "never"` on a live pane means no replacement.
        assert!(text.contains("no replacement"), "got {text}");
        // And the trigger contract: a fire restarts the child. The
        // README points at `?` for this, so `?` must actually say it.
        assert!(text.contains("restarts the child"), "got {text}");
    }

    #[test]
    fn the_live_help_is_absent_when_no_pane_is_live() {
        // The trigger-only dashboard must not gain the live section —
        // and the no-trigger, no-live case is already pinned
        // byte-identical below.
        let text = pane_help(&registry(true), &[]).join(" ");
        assert!(!text.contains("live panes:"), "got {text}");
    }

    #[test]
    fn the_help_explains_the_badge_and_names_the_fix() {
        // De-wrapped before matching. The section is wrapped by hand,
        // so a phrase the reader sees as one sentence can straddle a
        // line break — asserting on the wrapped bytes would pin the
        // wrap POINTS rather than the claim, and would fail on any
        // future rewrap that changed nothing a user can perceive. The
        // width is pinned separately, below, because that is a
        // different property.
        let lines = pane_help(&registry(true), &[]);
        let text = lines.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ");
        assert!(text.contains("looping"), "got {text}");
        // The two facts a user cannot guess: that the pane was left
        // running, and that a trigger fires on the timestamp rather
        // than on the bytes — so writing identical content still fires.
        assert!(text.contains("nothing has been stopped"), "got {text}");
        assert!(text.contains("mtime, not on content"), "got {text}");
        // And the third, which is about the badge's ABSENCE: the detector
        // can decline to answer, and a silent dashboard is not a clean
        // one. Asserted because a static string nothing reads is exactly
        // how this codebase has shipped a lie before.
        assert!(text.contains("no loop was proved"), "got {text}");
    }

    #[test]
    fn the_help_stays_inside_the_width_the_key_table_already_sets() {
        // `?` pages plain grouped text through the pager with no
        // wrapping engine behind it, so a line wider than the shipped
        // key table is one the pager has to chop. 74 is the widest row
        // that ships (`p  freeze the frame in place …`); this section
        // may reach it and must not pass it.
        for line in pane_help(&registry(true), &[])
            .into_iter()
            .chain(pane_help(&live_registry(), &[]))
            .chain(pane_help(&registry(false), &help_bindings()))
            .chain(pane_help(&registry(false), &[long_binding()]))
        {
            assert!(
                line.chars().count() <= 74,
                "{} cells: {line:?}",
                line.chars().count()
            );
        }
    }

    #[test]
    fn load_diagnostics_get_their_own_help_section_only_when_present() {
        // The `?` reference is the diagnostics channel: load-time
        // facts worth telling but not worth failing over. Absent
        // diagnostics add NOTHING — the byte-identity pin below stays
        // the proof for the common case.
        let noisy = registry(false).with_diagnostics(vec![
            "duplicate id \"a\" — refs bind to the first declaration".to_string(),
        ]);
        let lines = pane_help(&noisy, &[]);
        assert!(
            lines.contains(&"  diagnostics:".to_string()),
            "the section header appears: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("duplicate id \"a\"") && l.starts_with("    ")),
            "the diagnostic is listed, indented: {lines:?}"
        );
        let quiet = pane_help(&registry(false), &[]);
        assert!(
            !quiet.iter().any(|l| l.contains("diagnostics")),
            "no section when there is nothing to say: {quiet:?}"
        );
    }

    #[test]
    fn the_help_is_unchanged_when_no_pane_has_a_trigger() {
        // Byte-identity for the common case, asserted against the
        // literal shipped block rather than against a recomputation of
        // it: a dashboard with no triggers gains no help text at all,
        // and cannot gain any by a later edit without failing here.
        assert_eq!(
            pane_help(&registry(false), &[]),
            vec![
                String::new(),
                "  panes:".to_string(),
                "    a  every 5s".to_string(),
                "    b  every 5s".to_string(),
                String::new(),
                "  pane gestures (while the frame is live — not the `live` label):".to_string(),
                "    Tab, BackTab     cycle focus between panes — a zoom rides along".to_string(),
                "    Alt-h/j/k/l      move focus directionally".to_string(),
                "    Alt-1..9         jump straight to a numbered focusable pane".to_string(),
                "    Esc              peel one layer: zoom, focus, frame scroll".to_string(),
                "    Enter            zoom the focused pane; zoomed, page its body".to_string(),
                "    z                zoom the focused pane to the full frame".to_string(),
                "    Space            collapse the focused pane to its title row".to_string(),
                "    With a pane focused, the scroll keys and the wheel drive that".to_string(),
                "    pane's own window instead of the whole frame, and focusing a".to_string(),
                "    pane below the fold scrolls the frame to it. h/l shift".to_string(),
                "    nothing here: pane content is clipped to its box. While a".to_string(),
                "    focus is held, every focusable title counts itself in layout".to_string(),
                "    order; Alt-1..9 jumps to the first nine (from rest, too),".to_string(),
                "    and Tab reaches the rest of a larger board.".to_string(),
            ]
        );
        // Stated rather than implied: a board with no bindings gains
        // no section at all.
        let quiet = pane_help(&registry(false), &[]);
        assert!(
            !quiet.iter().any(|l| l.contains("key actions")),
            "{quiet:?}"
        );
    }

    /// The reference documents the keys this board answers, and a
    /// cursor is opt-in — so `s` appears exactly where pressing it does
    /// something. A reference that named a key the board hands to its
    /// own bindings would be worse than silent: it would be wrong.
    ///
    /// The DIFFERENCE is the assertion, not a second literal block. A
    /// copy of the whole reference here would go stale against the one
    /// above without either failing.
    #[test]
    fn the_reference_grows_the_cursor_rows_only_where_a_pane_asked_for_one() {
        let without = pane_help(&registry(false), &[]);
        let with = pane_help(&registry_with_a_cursor(), &[]);
        assert!(
            !without.iter().any(|l| l.contains("line cursor")),
            "a board with no cursor must not advertise the key: {without:?}"
        );
        let added: Vec<&String> = with.iter().filter(|l| !without.contains(l)).collect();
        assert_eq!(
            added,
            [
                "    s                put a line cursor in the focused pane, or drop it",
                "    Esc              peel one layer: cursor, zoom, focus, frame scroll",
                "    With a cursor up, the scroll keys move the cursor instead and",
                "    the pane's window follows it; a key action then reads the",
                "    marked line as RAT_SELECTION. The cursor holds its line when",
                "    the pane re-runs, and a paused frame ignores all of this.",
            ]
        );
        // Esc is REPLACED rather than added beside: the rung it peels
        // first only exists on a board that can hold a cursor, and two
        // Esc rows would describe two different keys.
        assert_eq!(
            with.iter().filter(|l| l.contains("    Esc ")).count(),
            1,
            "{with:?}"
        );
        // The rows keep their places rather than being appended in a
        // clump: `s` reads with the gestures, and the paragraph reads
        // with the prose.
        let at = |lines: &[String], needle: &str| {
            lines.iter().position(|l| l.contains(needle)).expect(needle)
        };
        assert!(at(&with, "    s   ") > at(&with, "Alt-1..9"));
        assert!(at(&with, "    s   ") < at(&with, "    Esc "));
        assert!(at(&with, "With a cursor up") > at(&with, "and Tab reaches"));
    }

    #[test]
    fn the_help_names_the_pane_gestures() {
        // Unconditional, unlike LIVE_HELP: every dashboard registry is
        // Composition::Panes, so the gestures always apply — proven by
        // asking a registry with no live pane and no trigger.
        let text = pane_help(&registry(false), &[]).join("\n");
        for needle in [
            "Tab",
            "BackTab",
            "Alt-h/j/k/l",
            "Alt-1..9",
            "Esc",
            "Enter",
            "z ",
            "Space",
            "focus",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in {text}");
        }
        // The retargeting sentence is a behavior change to keys the
        // shared reference already documents, so it must be stated here.
        assert!(text.contains("scroll keys"), "the retarget is unstated");
        // And the cursor row, on the board that has one. Not `"s"` or
        // `"s "`: both match incidentally, in `pane gestures`, in
        // `panes`, in half the prose. A phrase that can only come from
        // that row.
        let with_cursor = pane_help(&registry_with_a_cursor(), &[]).join("\n");
        assert!(with_cursor.contains("line cursor"), "{with_cursor}");
    }

    #[test]
    fn every_registered_template_validates() {
        // Over the REGISTRY, so a template added to `init` is a
        // template this test covers — a hand-copied list drifts the
        // moment someone adds a row and forgets the other file. Local
        // to this module because the registry is private and stays so.
        use crate::core::variables::resolve_partial;
        assert!(!TEMPLATES.is_empty());
        for template in TEMPLATES {
            let file = crate::core::dashboard_kdl::parse_styled(template.body, false)
                .unwrap_or_else(|err| panic!("{} parses: {err:#}", template.name));
            // A template binding one of rat's own keys is a board `init`
            // hands the reader pre-broken. Until this line, the only
            // thing standing between that and a shipped release was a
            // pair of integration tests that happen to shell out to
            // `check` — this is the whole registry, in a unit test.
            refuse_claimed_bindings(&file.bindings, file.takes_a_cursor())
                .unwrap_or_else(|err| panic!("{} binds a claimed key: {err:#}", template.name));
            // No commands run: a template's shell=#true variables are
            // Opaque here, exactly as under `rat dashboard check`.
            let partial = resolve_partial(&file.variables, &Bindings::default());
            file.audit_sites(&partial)
                .unwrap_or_else(|err| panic!("{} validates: {err:#}", template.name));
        }
    }

    #[test]
    fn the_review_template_exercises_a_derived_site() {
        // The positive form of an emptiness check: a blanket
        // `unchecked.is_empty()` across the registry would forbid the
        // derived-trigger path this template exists to demonstrate.
        // Non-empty AND naming `store` proves the demo is still there.
        use crate::core::variables::resolve_partial;
        let review = TEMPLATES
            .iter()
            .find(|t| t.name == "review")
            .expect("registered");
        let file = crate::core::dashboard_kdl::parse_styled(review.body, false).expect("parses");
        let partial = resolve_partial(&file.variables, &Bindings::default());
        let audit = file.audit_sites(&partial).expect("validates");
        assert!(
            !audit.unchecked.is_empty(),
            "the derived-site demo was removed"
        );
        assert!(
            audit
                .unchecked
                .iter()
                .any(|site| site.blockers.iter().any(|b| b == "sel" || b == "store")),
            "the skip traces to the derived store: {:?}",
            audit.unchecked
        );
    }

    #[test]
    fn no_template_carries_a_private_reference() {
        // Assembled rather than written: this file ships in the public
        // repo, and a literal here would be the very leak the
        // assertion forbids. Each fragment is inert on its own; only
        // the concatenation matches.
        assert!(!TEMPLATES.is_empty());
        let bare = [["gum", "bo"].concat(), ["point", "break"].concat()];
        let paths = ["/users/", "c:\\users", "plans/"];
        for template in TEMPLATES {
            let hay = template.body.to_lowercase();
            for token in &bare {
                assert!(
                    !hay.contains(token.as_str()),
                    "{} leaks a private name",
                    template.name
                );
                assert!(
                    !hay.contains(&format!(".{token}")),
                    "{} leaks a private path",
                    template.name
                );
            }
            for needle in paths {
                assert!(!hay.contains(needle), "{} leaks {needle:?}", template.name);
            }
        }
    }

    #[test]
    fn no_template_assumes_it_lives_in_a_clone() {
        // Scanned over the PARSED board, never the raw bytes: every
        // example's header comment carries its own
        // `rat dashboard examples/…` run line, which documents where
        // the file lives — a repo-relative path in a COMMAND value is
        // the runtime dependency this forbids. `panes-nested` is the
        // one real violator, which is why it is excluded from the
        // registry rather than rewritten.
        let mut values_scanned = 0usize;
        for template in TEMPLATES {
            let file = crate::core::dashboard_kdl::parse_styled(template.body, false)
                .unwrap_or_else(|err| panic!("{} parses: {err:#}", template.name));
            for decl in file.panes.iter().chain(std::iter::once(&file.defaults)) {
                let mut check_value = |value: &crate::core::template::Template| {
                    values_scanned += 1;
                    assert!(
                        !value.as_str().contains("examples/"),
                        "{} reaches into the repository: {:?}",
                        template.name,
                        value.as_str()
                    );
                };
                for value in decl.command.iter().flatten() {
                    check_value(value);
                }
                for value in decl.trigger.iter().flatten() {
                    check_value(value);
                }
                if let Some(body) = &decl.script {
                    check_value(body);
                }
            }
        }
        // The presence anchor: a parse that yielded nothing to check
        // would satisfy the loop vacuously.
        assert!(values_scanned > 0, "no command values were scanned");
    }

    #[test]
    fn every_template_name_is_unique_and_the_default_exists() {
        let mut seen: Vec<&str> = Vec::new();
        for template in TEMPLATES {
            assert!(
                !seen.contains(&template.name),
                "duplicate {}",
                template.name
            );
            assert!(!template.body.is_empty(), "{} is empty", template.name);
            assert!(
                !template.summary.is_empty(),
                "{} has no summary",
                template.name
            );
            seen.push(template.name);
        }
        assert!(seen.contains(&DEFAULT_TEMPLATE), "the default resolves");
    }

    /// The composed path, not a hand-assembled one: `validated()` is
    /// read_and_parse -> refuse_claimed_bindings -> finish_load, so
    /// "the example loads" already asserts that no binding names a
    /// claimed key, with the load-time error naming what the key
    /// already does. The assertions inspect the RESOLVED board, never
    /// the bytes: the example's header comment names every keyword the
    /// grammar has, so a substring check would pass on a board with no
    /// bindings at all.
    #[test]
    fn the_keys_example_declares_a_worked_binding() {
        use crate::core::dashboard_file::BindingOutput;
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/keys.kdl");
        let board = validated(std::path::Path::new(path), false, &Bindings::new())
            .expect("the example loads: parses, binds no claimed key, and resolves");
        let keys = &board.bindings;
        assert!(keys.len() >= 3, "{keys:?}");
        assert!(keys.iter().all(|b| !b.description.trim().is_empty()));
        assert_eq!(keys.iter().filter(|b| b.when.is_some()).count(), 1);
        assert_eq!(keys.iter().filter(|b| b.confirm.is_some()).count(), 1);
        // Every disposition the grammar defines appears somewhere in
        // the file, including the default one — an example that only
        // shows the default teaches that there is nothing to choose.
        for want in [
            BindingOutput::Hide,
            BindingOutput::Status,
            BindingOutput::Pager,
        ] {
            assert!(
                keys.iter().any(|b| b.output == want),
                "{want:?} unshown in {keys:?}"
            );
        }
    }

    #[test]
    fn the_report_shape_is_pinned() {
        use crate::core::variables::resolve_partial;
        let file = crate::core::dashboard_kdl::parse_styled(
            "variables {\n    plan \"/tmp/x\"\n    store \"git rev-parse\" shell=#true\n    sel \"{{store}}/s\"\n}\n\npane \"events\" {\n    height 3\n    command \"true\"\n    trigger \"file:{{sel}}\"\n    title \"{{plan}}\"\n}\n",
            false,
        )
        .expect("parses");
        let overrides = Bindings::new();
        let partial = resolve_partial(&file.variables, &overrides);
        let audit = file.audit_sites(&partial).expect("audits");
        let rows: Vec<SkipRow> = audit
            .unchecked
            .iter()
            .map(|site| SkipRow {
                roots: site
                    .blockers
                    .iter()
                    .flat_map(|blocker| {
                        crate::core::variables::opaque_roots(&file.variables, &partial, blocker)
                    })
                    .collect(),
                site: UncheckedSite {
                    origin: site.origin.clone(),
                    key: site.key,
                    blockers: site.blockers.clone(),
                },
            })
            .collect();
        let report = render_report(
            &file,
            &partial,
            &overrides,
            &rows,
            std::path::Path::new("board.kdl"),
        );
        // The verdict line first, so `head -1` is a summary.
        assert!(
            report.starts_with("board.kdl: 1 pane, 3 variables — ok\n"),
            "{report}"
        );
        // The effective tier, not the declared form: `sel` is a
        // constant promoted by its reference to a command.
        assert!(report.contains("sel"), "{report}");
        let sel_line = report
            .lines()
            .find(|line| line.trim_start().starts_with("sel"))
            .expect("sel row");
        assert!(
            sel_line.contains("once at load"),
            "the promotion shows: {sel_line}"
        );
        assert!(sel_line.contains("opaque"), "{sel_line}");
        // The skip row shows the chain: the direct reference is a
        // tainted constant, the root is the command.
        assert!(
            report.contains("sel → store, derived by a command"),
            "{report}"
        );
        assert!(report.contains("pane \"events\" trigger"), "{report}");
    }

    #[test]
    fn an_override_splits_at_the_first_equals() {
        let map = parse_overrides(&[
            "plan=/tmp/plans/0028".to_string(),
            "flags=--since=yesterday".to_string(),
            "empty=".to_string(),
        ])
        .expect("parses");
        assert_eq!(map.get("plan").map(String::as_str), Some("/tmp/plans/0028"));
        // The FIRST `=` splits: a value may contain as many as it likes.
        assert_eq!(
            map.get("flags").map(String::as_str),
            Some("--since=yesterday")
        );
        // An empty value is the author saying so, exactly as `limit ""`
        // is — unlike a command's empty OUTPUT, which is a derivation
        // failure (INV-4.2).
        assert_eq!(map.get("empty").map(String::as_str), Some(""));
    }

    #[test]
    fn a_malformed_override_teaches_the_spelling() {
        for (arg, needle) in [
            ("plan", "-v takes `name=value`"),
            ("=/tmp/x", "a variable's name starts with a letter or `_`"),
            ("9lives=x", "a variable's name starts with a letter or `_`"),
            ("a b=x", "a variable's name starts with a letter or `_`"),
        ] {
            let err = format!("{:#}", parse_overrides(&[arg.to_string()]).expect_err(arg));
            assert!(err.contains(needle), "{arg} → {err}");
        }
    }

    #[test]
    fn the_same_name_is_given_a_value_once() {
        // The document's own rule, one level out: last-wins is invisible
        // in a long command line the same way it is invisible in a long
        // pane block (`record` in the walk).
        let err = format!(
            "{:#}",
            parse_overrides(&["plan=a".to_string(), "plan=b".to_string()]).expect_err("twice")
        );
        assert!(err.contains("-v plan is given twice"), "got {err}");
    }
    // ─── The claimed-key refusal ────────────────────────────────────

    fn binding(key: crate::ui::key::Key) -> crate::core::dashboard_file::KeyBindingDecl {
        use crate::core::template::Template;
        crate::core::dashboard_file::KeyBindingDecl {
            key,
            spelling: crate::core::key_spelling::spelling_of(key),
            description: Template::extract("x"),
            program: crate::core::dashboard_file::BindingProgram::Argv(vec![Template::extract(
                "true",
            )]),
            shell: None,
            when: None,
            output: None,
            confirm: None,
        }
    }

    #[test]
    fn a_binding_on_one_of_rats_own_keys_is_refused_by_name() {
        use crate::ui::key::Key;
        let err = format!(
            "{:#}",
            refuse_claimed_bindings(&[binding(Key::Char('j'))], true).unwrap_err()
        );
        assert_eq!(
            err,
            "key \"j\": `j` is one of rat's own keys — it scrolls down one line. \
             Press `?` on any board for the full list, then pick a key it does not name"
        );
    }

    #[test]
    fn a_board_that_asked_for_a_cursor_can_no_longer_bind_the_cursor_key() {
        // The load-time half of the collision, through the surface a
        // board actually hits. The derivation and the refusal are
        // already proved to agree over the whole spellable space; what
        // this pins is the SENTENCE the author reads.
        use crate::ui::key::Key;
        let err = format!(
            "{:#}",
            refuse_claimed_bindings(&[binding(Key::Char('s'))], true).unwrap_err()
        );
        assert!(err.starts_with("key \"s\":"), "{err}");
        assert!(err.contains("line cursor"), "{err}");
    }

    #[test]
    fn a_board_with_no_cursor_keeps_the_cursor_key_for_itself() {
        // A cursor is opt-in, so on a board where no pane asked for one
        // the key does nothing, and taking a letter away from every
        // board to serve the few that use it is a cost with no matching
        // benefit.
        use crate::ui::key::Key;
        refuse_claimed_bindings(&[binding(Key::Char('s'))], false)
            .expect("a key the loop will not answer is the board's to bind");
        // Nothing else moved with it: the keys claimed for gestures that
        // work on every board stay claimed on every board.
        assert!(
            refuse_claimed_bindings(&[binding(Key::Char('j'))], false).is_err(),
            "the scroll keys are claimed whatever the panes declared"
        );
    }

    #[test]
    fn a_binding_on_a_free_key_is_accepted() {
        use crate::ui::key::Key;
        refuse_claimed_bindings(&[binding(Key::Char('a')), binding(Key::Alt('x'))], true)
            .expect("both free");
    }

    #[test]
    fn the_refusal_fires_for_exactly_the_keys_the_derivation_claims() {
        // The matrix again, through the surface a board actually hits —
        // so a refusal that reads the derivation correctly but applies it
        // to the wrong field cannot pass. Run under BOTH answers to the
        // cursor question, because the refusal and the loop have to
        // agree about `s` in each: a key refused at load that the loop
        // would have handed to the board costs the author a letter for
        // nothing, and the reverse is a binding that never fires.
        for cursor in [false, true] {
            for key in crate::core::key_spelling::ascii_spellable() {
                assert_eq!(
                    refuse_claimed_bindings(&[binding(key)], cursor).is_err(),
                    crate::commands::watch::builtin_key(key, cursor).is_some(),
                    "{key:?} on a board where cursor={cursor}"
                );
            }
        }
    }

    #[test]
    fn the_first_claimed_binding_in_declaration_order_is_the_one_reported() {
        use crate::ui::key::Key;
        let err = format!(
            "{:#}",
            refuse_claimed_bindings(
                &[
                    binding(Key::Char('a')),
                    binding(Key::Char('q')),
                    binding(Key::Char('j'))
                ],
                true
            )
            .unwrap_err()
        );
        assert!(err.starts_with("key \"q\":"), "{err}");
    }
    // ─── The binding help section ───────────────────────────────────

    fn shown(key: char, description: &str) -> KeyBinding {
        KeyBinding {
            key: crate::ui::key::Key::Char(key),
            spelling: key.to_string(),
            description: description.to_string(),
            program: crate::core::dashboard_file::BindingProgram::Argv(vec![
                crate::core::template::Template::extract("true"),
            ]),
            shell: ShellMode::Direct,
            when: None,
            when_shell: None,
            output: crate::core::dashboard_file::BindingOutput::Status,
            confirm: None,
        }
    }

    /// Two bindings, one plain and one that confirms — the smallest
    /// board that exercises both line shapes.
    fn help_bindings() -> Vec<KeyBinding> {
        let mut confirming = shown('a', "assess this change");
        confirming.confirm = Some("Really?".to_string());
        vec![shown('r', "rerun the suite"), confirming]
    }

    fn long_binding() -> KeyBinding {
        shown('e', &"x".repeat(200))
    }

    #[test]
    fn a_declared_binding_appears_in_the_help_with_its_key_and_its_description() {
        let lines = pane_help(&registry(false), &help_bindings());
        assert!(
            lines.iter().any(|l| l.contains("  key actions")),
            "{lines:?}"
        );
        // The EXACT line: the column position is what drifts, and it is
        // the one thing a reader compares against the tables above it.
        assert!(
            lines.contains(&"    r                rerun the suite".to_string()),
            "{lines:?}"
        );
    }

    #[test]
    fn bindings_are_listed_in_declaration_order() {
        // Never sorted: the author's order carries meaning — a review
        // board groups its verdict keys together.
        let bindings = vec![
            shown('x', "third in the alphabet, first declared"),
            shown('a', "first in the alphabet, second declared"),
            shown('r', "middle, last declared"),
        ];
        let lines = pane_help(&registry(false), &bindings);
        let position = |needle: &str| {
            lines
                .iter()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("missing {needle:?} in {lines:?}"))
        };
        assert!(position("third in the alphabet") < position("first in the alphabet"));
        assert!(position("first in the alphabet") < position("middle, last declared"));
    }

    #[test]
    fn a_confirming_binding_says_so_and_a_guarded_one_does_not() {
        // Annotate what changes what the keypress COSTS the reader: a
        // confirm interrupts. A `when` changes only whether the command
        // runs, and its decline names the key at the moment it happens
        // — a better surface than a static note that would sit on
        // nearly every line of a real console.
        let mut guarded = shown('n', "advance the queue");
        guarded.when = Some(crate::core::template::Template::extract("test -s ./x"));
        guarded.when_shell = Some(ShellMode::Platform);
        let mut confirming = shown('a', "assess this change");
        confirming.confirm = Some("Really?".to_string());
        let lines = pane_help(&registry(false), &[guarded, confirming]);
        let line_for = |needle: &str| {
            lines
                .iter()
                .find(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("missing {needle:?} in {lines:?}"))
        };
        assert!(
            line_for("assess this change").ends_with("(asks before running)"),
            "{lines:?}"
        );
        assert!(
            !line_for("advance the queue").contains('('),
            "a `when` earns no annotation: {lines:?}"
        );
    }

    #[test]
    fn a_long_description_is_truncated_rather_than_overflowing_the_help_width() {
        let lines = pane_help(&registry(false), &[long_binding()]);
        let line = lines
            .iter()
            .find(|l| l.starts_with("    e"))
            .expect("the binding's line exists — the key survives the cut");
        assert!(
            crate::core::measure::display_width(line) <= 74,
            "{} cells: {line:?}",
            crate::core::measure::display_width(line)
        );
        assert!(
            line.ends_with(crate::core::measure::ELLIPSIS),
            "cut through truncate_display: {line:?}"
        );
    }
}
