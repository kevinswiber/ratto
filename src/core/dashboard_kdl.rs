//! The KDL constructor. A `KdlDocument` walk to [`DashboardFile`] —
//! parsing only; every rule lives once, in `into_registry`. KDL v2
//! grammar (`#true` / `#false` for booleans).
//!
//! # The rule
//!
//! Every key a `pane` or `defaults` block accepts holds exactly one
//! value, so it may be written as a property or as a child node,
//! author's choice. `command`'s argv and `trigger`'s specs hold LISTS
//! and have no property spelling, because a KDL property holds exactly
//! one value. `row`, `column`, `gap` and `row-gap` are not keys —
//! containers hold only cells, and `gap`/`row-gap` are the dashboard's,
//! written once at the top level.
//!
//! It is written here, and in `examples/panes.kdl`'s header, because a
//! rule that is real, uniform and mechanically enforced still reads as
//! arbitrary to someone who has only ever met it as an error message
//! (zellij shipped this exact seam undocumented — their #3629).
//!
//! # Site accounting (INV-6)
//!
//! "Every string site is validated" is only checkable if the sites are
//! enumerated. Every row is either validated against the declared
//! variables or has a stated reason it cannot hold a reference:
//!
//! | Key | Shape | Reaches | String site? |
//! |---|---|---|---|
//! | `command`, `trigger` | `List` | `many_text` | yes |
//! | `script`, `interval`, `trigger-debounce`, `width`, `overflow`, `border`, `padding`, `title` | `Text` | `prop_text` / `one_text` | yes |
//! | `shell` | `FlagOrText` | `shell_decl` → `ShellDecl::Named(Template)` | yes, in its string arm |
//! | `height` | `Count` | `prop_count` / `one_count` | no — integers never reach a string (INV-3) |
//! | `chrome`, `focusable`, `live` | `Flag` | `prop_flag` / `one_flag` | no |
//! | document `title` text | — | `title_field` | yes |
//! | document `title` `ref` | — | `title_field` | **refused** — an id is identity (INV-3) |
//! | document `gap`, `row-gap` | — | `usize_field` | no — integers |
//! | pane id positional | — | `one_id` | **refused** by the existing charset check |
//! | `variables` values | — | `variables_block` | yes — validated by `classify` |

use anyhow::{anyhow, bail};

use crate::core::dashboard_file::{DashboardFile, LayoutDecl, PaneDecl};
use crate::core::registry::ShellDecl;
use crate::core::template::{Template, is_reference_name};
use crate::core::variables::{Tier, VarSource, Variable, VariableBlock};

/// The one function that puts a key's value on the declaration. The
/// variant IS the key's shape: it says what the value looks like, and
/// therefore where it may be written — a KDL property holds exactly one
/// value, so only `List` lacks a property spelling.
enum Set {
    Text(fn(&mut PaneDecl, Template)),
    Count(fn(&mut PaneDecl, i128, &Ctx<'_>) -> anyhow::Result<()>),
    Flag(fn(&mut PaneDecl, bool)),
    List(fn(&mut PaneDecl, Vec<Template>, &Ctx<'_>) -> anyhow::Result<()>),
    /// `#true`, `#false`, or one string naming the choice — a switch
    /// and a choice in one key. `shell` is the only key with this
    /// shape, so the payload is its own type; a second such key would
    /// earn a shape-neutral one.
    FlagOrText(fn(&mut PaneDecl, ShellDecl)),
}

impl Set {
    /// A list key has no property spelling; every other shape has both.
    fn takes_a_property(&self) -> bool {
        !matches!(self, Set::List(_))
    }
}

/// One key a `pane` or `defaults` block accepts: its name, the example
/// every teaching error shows, and the one function that applies it.
/// Dispatch, property legality and every accepted-set list read THIS —
/// a new key is one row here and nothing else.
struct Key {
    name: &'static str,
    example: &'static str,
    set: Set,
}

impl Key {
    /// KDL writes a property as `key=value`, so the one example serves
    /// both positions. (List keys have no property spelling and never
    /// reach here.)
    fn property_example(&self) -> String {
        self.example.replacen(' ', "=", 1)
    }
}

const PANE_KEYS: &[Key] = &[
    Key {
        name: "command",
        example: r#"command "git" "log""#,
        set: Set::List(set_command),
    },
    Key {
        name: "script",
        // `r##`: with one hash the `"#` inside `"#!` would terminate
        // the raw string early.
        example: r##"script "#!/bin/sh\necho hi""##,
        set: Set::Text(|d, v| d.script = Some(v)),
    },
    Key {
        name: "shell",
        example: "shell #true",
        set: Set::FlagOrText(|d, v| d.shell = Some(v)),
    },
    Key {
        name: "interval",
        example: r#"interval "5s""#,
        set: Set::Text(|d, v| d.interval = Some(v)),
    },
    Key {
        name: "trigger",
        example: r#"trigger "file:./stamp""#,
        set: Set::List(|d, v, _| {
            d.trigger = Some(v);
            Ok(())
        }),
    },
    Key {
        name: "trigger-debounce",
        example: r#"trigger-debounce "250ms""#,
        set: Set::Text(|d, v| d.trigger_debounce = Some(v)),
    },
    Key {
        name: "height",
        example: "height 7",
        set: Set::Count(set_height),
    },
    Key {
        name: "width",
        example: r#"width "2fr""#,
        set: Set::Text(|d, v| d.width = Some(v)),
    },
    Key {
        name: "overflow",
        example: r#"overflow "keep-bottom""#,
        set: Set::Text(|d, v| d.overflow = Some(v)),
    },
    Key {
        name: "border",
        example: r#"border "rounded""#,
        set: Set::Text(|d, v| d.border = Some(v)),
    },
    Key {
        name: "padding",
        example: r#"padding "0 1""#,
        set: Set::Text(|d, v| d.padding = Some(v)),
    },
    Key {
        name: "title",
        example: r#"title "Recent commits""#,
        set: Set::Text(|d, v| d.title = Some(v)),
    },
    Key {
        name: "chrome",
        example: "chrome #false",
        set: Set::Flag(|d, v| d.chrome = Some(v)),
    },
    Key {
        name: "focusable",
        example: "focusable #false",
        set: Set::Flag(|d, v| d.focusable = Some(v)),
    },
    Key {
        name: "live",
        example: "live #true",
        set: Set::Flag(|d, v| d.live = Some(v)),
    },
];

/// What a setter needs beyond the value: the phrase every teaching
/// error opens with, and the shell mode in force where the command was
/// written — the one thing the parser resolves against defaults.
struct Ctx<'a> {
    at: &'a str,
    shell: bool,
}

fn set_command(decl: &mut PaneDecl, argv: Vec<Template>, ctx: &Ctx<'_>) -> anyhow::Result<()> {
    decl.command = Some(match argv.as_slice() {
        // One word under `shell` stays one word.
        [script] if ctx.shell => vec![script.clone()],
        // An unbalanced string is a parse error naming the pane — never
        // a one-word fallback that survives to a spawn.
        //
        // The split happens at PARSE, on TEMPLATE text, and each word
        // is re-recorded under the whole value's flavor — so an
        // expansion lands INSIDE the word that held it and never
        // creates a new argv element (INV-7 sub-rule 1).
        [line] => line.reslice(
            shell_words::split(line.as_str())
                .map_err(|err| anyhow!("{}: command has unbalanced quoting ({err})", ctx.at))?,
        ),
        argv => argv.to_vec(),
    });
    Ok(())
}

fn set_height(decl: &mut PaneDecl, cells: i128, ctx: &Ctx<'_>) -> anyhow::Result<()> {
    decl.height = Some(u16::try_from(cells).map_err(|_| {
        anyhow!(
            "{}: height must be a non-negative integer (max 65535)",
            ctx.at
        )
    })?);
    Ok(())
}

fn key(name: &str) -> Option<&'static Key> {
    PANE_KEYS.iter().find(|k| k.name == name)
}

/// The keys legal in the position the error is complaining about.
fn key_list(property_position: bool) -> String {
    PANE_KEYS
        .iter()
        .filter(|k| !property_position || k.set.takes_a_property())
        .map(|k| k.name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Every shape error is generated from the key's own example, so there
/// is no per-key error text to keep in step with the table.
fn shape_err(k: &Key, at: &str) -> anyhow::Error {
    anyhow!(
        "{at}: `{}` takes {} — write `{}`",
        k.name,
        takes(k),
        k.example
    )
}

/// The same complaint against the property spelling, so the fix it
/// shows is the one the user was reaching for.
fn prop_shape_err(k: &Key, at: &str) -> anyhow::Error {
    anyhow!(
        "{at}: `{}` takes {} — write `{}`",
        k.name,
        takes(k),
        k.property_example()
    )
}

fn takes(k: &Key) -> &'static str {
    match k.set {
        Set::Text(_) => "one string",
        Set::Count(_) => "one integer",
        Set::Flag(_) => "#true or #false",
        Set::List(_) => "one or more strings",
        Set::FlagOrText(_) => "#true, #false, or one string",
    }
}

/// I-55 at the document level: a dashboard has one gap and one set of
/// defaults, so a second declaration is not a refinement of the first —
/// it is a line the reader will believe and the parser would drop.
fn declared_once(name: &'static str, seen: &mut Vec<&'static str>) -> anyhow::Result<()> {
    if seen.contains(&name) {
        bail!("`{name}` is declared twice — a dashboard declares it once");
    }
    seen.push(name);
    Ok(())
}

/// A KDL type annotation never means anything to this grammar, wherever
/// it is hung — on the key node, or on the value the key holds (D-7).
fn refuse_annotation(ty: Option<&kdl::KdlIdentifier>, key: &str, at: &str) -> anyhow::Result<()> {
    match ty {
        Some(ty) => bail!(
            "{at}: the ({}) type annotation on `{key}` has no meaning here — remove it",
            ty.value()
        ),
        None => Ok(()),
    }
}

/// A key node carries its value and NOTHING else. A property hung on
/// it, a block under it, or an annotation anywhere on it is a token
/// with nowhere to go — the same silent discard I-52 closes one level
/// up, one level down.
fn only_a_value(node: &kdl::KdlNode, k: &Key, at: &str) -> anyhow::Result<()> {
    refuse_annotation(node.ty(), k.name, at)?;
    for entry in node.entries() {
        match entry.name() {
            Some(prop) => bail!(
                "{at}: `{}` takes {}, but {:?} is set — write `{}`",
                k.name,
                takes(k),
                prop.value(),
                k.example
            ),
            None => refuse_annotation(entry.ty(), k.name, at)?,
        }
    }
    // `is_some`, not "has nodes": an EMPTY block is still a block the
    // author wrote, and this very message tells them the key holds
    // none. A block they slashdashed OUT never reaches here — the crate
    // strips it before the walk, which is what keeps commenting-out
    // working (D-8).
    if node.children().is_some() {
        bail!(
            "{at}: `{}` takes {} and holds no block — write `{}`",
            k.name,
            takes(k),
            k.example
        );
    }
    Ok(())
}

/// One key, one place, once: a key written twice on the same block —
/// two properties, two child nodes, or one of each — is an error.
/// Last-wins is invisible to a reader scanning a long pane block.
fn record(seen: &mut Vec<&'static str>, k: &'static Key, at: &str) -> anyhow::Result<()> {
    if seen.contains(&k.name) {
        bail!(
            "{at}: `{}` is declared twice — declare it once, as a property or a child node",
            k.name
        );
    }
    seen.push(k.name);
    Ok(())
}

/// Place a byte offset into 1-based (line, column). The offset WALKS
/// bytes while the column COUNTS chars: kdl 6.7.1's diagnostic spans
/// are byte-indexed despite their doc comment saying chars — winnow's
/// `LocatingSlice` measures `offset_from` in bytes. A `\r` is not a
/// column (CRLF turns the line at its `\n`), a mid-char offset lands
/// after that char, and an offset past the end clamps to wherever the
/// walk stops — nothing here can panic or index out of bounds.
fn line_column(text: &str, offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut column = 1;
    for (i, ch) in text.char_indices() {
        if i >= offset {
            break;
        }
        match ch {
            '\n' => {
                line += 1;
                column = 1;
            }
            '\r' => {}
            _ => column += 1,
        }
    }
    (line, column)
}

/// The head line: `line N, column M: <message>` — the greppable form
/// scripts and logs key on. Upstream's message ships verbatim:
/// rewriting it is string surgery against a crate that can reword on
/// a patch bump. Help and the other diagnostics are not here — they
/// live in the miette snippet blocks `syntax_error` renders below,
/// which echo and point into the source at a bounded width (the
/// budgeted echo the original one-line-only rule was waiting for).
fn syntax_error_text(line: usize, column: usize, message: Option<&str>) -> String {
    format!(
        "line {line}, column {column}: {}",
        message.unwrap_or("invalid KDL")
    )
}

/// The placed error for a document that failed to parse: the earliest
/// diagnostic by span offset heads the message — the first failure is
/// the one the author can act on — and EVERY diagnostic then renders
/// its own miette snippet block, pointing into the source the way
/// rustc would, with upstream's help text inside the block. An error
/// with no diagnostics keeps the crate's own sentence verbatim;
/// fabricating a position would point at nothing.
///
/// Rendered per-DIAGNOSTIC rather than through the `KdlError` wrapper:
/// the wrapper's own report leads with its constant "Failed to parse
/// KDL document" sentence and an `Error:` separator per related item —
/// noise that says nothing the blocks do not. Unpinned by design: the
/// block glyphs are miette's to change; ours is the head line and the
/// fact that the source is echoed.
fn syntax_error(text: &str, err: &kdl::KdlError, colored: bool) -> anyhow::Error {
    use std::fmt::Write;
    let Some(first) = err.diagnostics.iter().min_by_key(|d| d.span.offset()) else {
        return anyhow!("{err}");
    };
    let (line, column) = line_column(text, first.span.offset());
    let mut message = syntax_error_text(line, column, first.message.as_deref());
    // The caller's stream decides the theme — never an env sniff here,
    // so `--color` and `NO_COLOR` keep their one authority. Colored is
    // plain 16-color ANSI (rustc's own choice): right at every depth,
    // degrading nowhere. The width is fixed either way: this renders
    // into an anyhow message printed by `rat: {err:#}`, where no
    // terminal re-measure reaches.
    let theme = if colored {
        miette::GraphicalTheme {
            characters: miette::ThemeCharacters::unicode(),
            styles: miette::ThemeStyles::ansi(),
        }
    } else {
        miette::GraphicalTheme::unicode_nocolor()
    };
    let handler = miette::GraphicalReportHandler::new_themed(theme).with_width(80);
    let mut blocks: Vec<&kdl::KdlDiagnostic> = err.diagnostics.iter().collect();
    blocks.sort_by_key(|d| d.span.offset());
    for diagnostic in blocks {
        let mut block = String::new();
        if handler.render_report(&mut block, diagnostic).is_ok() {
            let _ = write!(message, "\n{}", block.trim_end());
        }
    }
    anyhow!("{message}")
}

/// The test suite's spelling: `parse_styled` with the plain theme,
/// so hundreds of error-byte assertions never mention color.
#[cfg(test)]
fn parse(text: &str) -> anyhow::Result<DashboardFile> {
    parse_styled(text, false)
}

/// The document: settings, then the tree. A pane is declared inside the
/// `row`/`column` that places it.
///
/// The lift is parser-only. A pane's block becomes a `PaneDecl` in
/// `panes` — in document order, which is the order `SourceId`s are
/// handed out — and its name becomes a `LayoutDecl::Pane` in the tree.
/// `DashboardFile` never learns where the panes were written, so
/// `into_registry` stays the one validation path.
///
/// `colored` styles the error snippets for the caller's stream: it
/// says the reader has a color-capable terminal (the caller's
/// `ColorProfile` verdict — profile detection already folds in
/// `--color`, `NO_COLOR`, `CI`, and the stream's ttyness). The parse
/// outcome is identical either way.
pub fn parse_styled(text: &str, colored: bool) -> anyhow::Result<DashboardFile> {
    let doc: kdl::KdlDocument = text
        .parse()
        .map_err(|err| syntax_error(text, &err, colored))?;
    let mut file = DashboardFile::default();
    // The tree is walked AFTER the whole first pass, so a `defaults`
    // node anywhere in the document still supplies the `shell` the
    // command split depends on.
    let mut tree: Vec<&kdl::KdlNode> = Vec::new();
    let mut settings: Vec<&'static str> = Vec::new();
    let mut variables_node: Option<&kdl::KdlNode> = None;
    let mut defaults_node: Option<&kdl::KdlNode> = None;
    let mut title_node: Option<&kdl::KdlNode> = None;
    for node in doc.nodes() {
        match node.name().value() {
            "variables" => {
                declared_once("variables", &mut settings)?;
                variables_node = Some(node);
            }
            "title" => {
                declared_once("title", &mut settings)?;
                title_node = Some(node);
            }
            // `gap` / `row-gap` hold integers and can never hold a
            // reference (INV-3), so they stay in the pass.
            "gap" => {
                declared_once("gap", &mut settings)?;
                file.gap = Some(usize_field(node, "gap")?);
            }
            "row-gap" => {
                declared_once("row-gap", &mut settings)?;
                file.row_gap = Some(usize_field(node, "row-gap")?);
            }
            "defaults" => {
                declared_once("defaults", &mut settings)?;
                defaults_node = Some(node);
            }
            "pane" | "row" | "column" => tree.push(node),
            // Placement used to live in its own block, so a reader who
            // writes one is not guessing — they know a grammar that was
            // real. The error owes them the spelling that replaced it.
            "layout" => bail!(
                "there is no `layout` block — a pane is declared inside the row or column \
                 that places it: write `row {{ pane \"log\" {{ … }} pane \"branch\" {{ … }} }}`"
            ),
            other => {
                bail!(
                    "unknown node {other:?} — a dashboard's top level takes \
                     title, gap, row-gap, variables, defaults, pane, row, or column"
                )
            }
        }
    }
    // The block is built BEFORE any string site is read: position
    // carries no meaning in this format (INV-3), so a `variables`
    // block written at the BOTTOM must answer for a `{{name}}` written
    // at the top.
    let variables = match variables_node {
        Some(node) => variables_block(node, text, colored)?,
        None => VariableBlock::default(),
    };
    let load = Load {
        text,
        colored,
        vars: &variables,
    };
    if let Some(node) = title_node {
        file.title = Some(title_field(node, &load)?);
    }
    if let Some(node) = defaults_node {
        refuse_annotation(node.ty(), "defaults", "defaults")?;
        let values = positional(node);
        if !values.is_empty() {
            // A bare argument that names a key gets the spelling the
            // author was reaching for; one that names a document
            // setting gets sent to the top level. Only a string naming
            // neither keeps the positional refusal.
            if let Some(k) = stray_key(&values) {
                return Err(stray_key_err("defaults", k, &values));
            }
            if let Some(name) = stray_setting(&values) {
                return Err(stray_setting_err("defaults", name, &values));
            }
            bail!("defaults takes no id — it holds the keys every pane inherits");
        }
        file.defaults = pane_block(node, None, false, &load)?;
    }
    // The split cares whether a shell is involved, never which one.
    let default_shell = file
        .defaults
        .shell
        .as_ref()
        .is_some_and(ShellDecl::runs_a_shell);
    let mut panes = Vec::new();
    let mut items = Vec::with_capacity(tree.len());
    for (index, node) in tree.iter().enumerate() {
        let label = cell_label(None, node, index);
        items.push(inline_node(node, &label, default_shell, &mut panes, &load)?.normalized());
    }
    file.variables = variables;
    file.panes = panes;
    // Placement is STRUCTURAL, so the layout is never absent — the top
    // level IS the dashboard's column, and a file of bare panes states
    // that column explicitly (D-2). It resolves to what an absent layout
    // always resolved to.
    file.layout = Some(items);
    Ok(file)
}

/// Where a cell sits, as a breadcrumb — `row #1 > pane #2`. Position is
/// all a container knows about its cells, and often all a teaching error
/// has to point with, since a pane may not have named itself yet.
fn cell_label(inside: Option<&str>, node: &kdl::KdlNode, index: usize) -> String {
    let here = format!("{} #{}", node.name().value(), index + 1);
    match inside {
        Some(path) => format!("{path} > {here}"),
        None => here,
    }
}

/// One node of the inline tree, lifting every pane it meets into
/// `panes`. Its name has already been screened as a cell — by the
/// top-level match in `parse_inline`, or by the container below.
fn inline_node(
    node: &kdl::KdlNode,
    label: &str,
    default_shell: bool,
    panes: &mut Vec<PaneDecl>,
    load: &Load<'_>,
) -> anyhow::Result<LayoutDecl> {
    let kind = node.name().value();
    if kind == "pane" {
        // The same name reader the flat list used: a name is not an
        // internal handle, so exactly one string, unannotated.
        let name = one_id(node, label)?;
        panes.push(pane_block(node, Some(name.clone()), default_shell, load)?);
        return Ok(LayoutDecl::Pane(name));
    }
    refuse_annotation(node.ty(), kind, label)?;
    refuse_container_properties(node, label, container_kind(node))?;
    // A bare name here would be the old spelling leaking in: this row's
    // cells ARE the panes, and accepting a name would put one pane's
    // declaration back in two places. A bare token that names a
    // setting or a pane key gets a teaching answer first — the
    // setting's, then the key's, since `gap` on a row is the nearer
    // miss.
    let values = positional(node);
    if !values.is_empty() {
        if let Some(name) = stray_setting(&values) {
            return Err(stray_setting_err(label, name, &values));
        }
        if let Some(k) = stray_key(&values) {
            bail!(
                "{label}: `{}` is a pane's key — write it on a `pane` block inside this {kind}",
                k.name
            );
        }
        bail!(
            "{label}: a {kind} holds `pane` blocks, not pane ids — \
             declare the pane where it sits, like `{kind} {{ pane \"log\" {{ … }} }}`"
        );
    }
    let cells = node
        .children()
        .map(kdl::KdlDocument::nodes)
        .unwrap_or_default();
    if cells.is_empty() {
        bail!("{label}: this {kind} is empty — put at least one pane in it");
    }
    let mut decls = Vec::with_capacity(cells.len());
    for (index, cell) in cells.iter().enumerate() {
        let inner = cell_label(Some(label), cell, index);
        // What may be a cell is the CONTAINER's rule, so the container
        // is what the error names — `defaults` lands here too, since it
        // is the document's settings and has no position in the geometry.
        let cell_kind = cell.name().value();
        if !matches!(cell_kind, "pane" | "row" | "column") {
            bail!(
                "{inner}: unknown node {cell_kind:?} — {} holds `pane`, `row`, and `column` blocks",
                container_kind(node)
            );
        }
        decls.push(inline_node(cell, &inner, default_shell, panes, load)?);
    }
    Ok(if kind == "row" {
        LayoutDecl::Row(decls)
    } else {
        LayoutDecl::Column(decls)
    })
}

/// One `pane "name" { … }` or `defaults { … }` block. The block's own
/// `shell` is read FIRST because the command split depends on it —
/// `shell` is the one thing the parser resolves against defaults.
fn pane_block(
    node: &kdl::KdlNode,
    id: Option<String>,
    default_shell: bool,
    load: &Load<'_>,
) -> anyhow::Result<PaneDecl> {
    let at = match id.as_deref() {
        Some(name) => format!("pane {name:?}"),
        None => "defaults".to_string(),
    };
    let shell = peek_shell(node, &at)?;
    let ctx = Ctx {
        at: &at,
        shell: shell
            .as_ref()
            .map_or(default_shell, ShellDecl::runs_a_shell),
    };
    let mut decl = PaneDecl {
        id,
        ..PaneDecl::default()
    };
    let mut seen: Vec<&'static str> = Vec::new();

    // Properties first, and NOT behind the children lookup: a braceless
    // `pane "a" height=3` has no children at all.
    for entry in node.entries() {
        let Some(prop) = entry.name() else {
            continue; // positional: the pane's own name
        };
        let prop = prop.value();
        if let Some(ty) = entry.ty() {
            bail!(
                "{at}: the ({}) type annotation on `{prop}` has no meaning here — remove it",
                ty.value()
            );
        }
        let Some(k) = key(prop) else {
            bail!(
                "{at}: unknown property {prop:?} — a pane's keys with a property spelling are {}",
                key_list(true)
            );
        };
        record(&mut seen, k, &at)?;
        match k.set {
            Set::List(_) => bail!(
                "{at}: `{}` holds a list, so it must be a child node — write `{}` inside the block",
                k.name,
                k.example
            ),
            Set::Text(set) => set(&mut decl, prop_text(entry, k, &at, load)?),
            Set::Count(set) => set(&mut decl, prop_count(entry.value(), k, &at)?, &ctx)?,
            Set::Flag(set) => set(&mut decl, prop_flag(entry.value(), k, &at)?),
            Set::FlagOrText(set) => set(&mut decl, prop_mode(entry, k, &at, load)?),
        }
    }

    for child in node
        .children()
        .map(kdl::KdlDocument::nodes)
        .unwrap_or_default()
    {
        let name = child.name().value();
        let Some(k) = key(name) else {
            bail!(
                "{at}: unknown node {name:?} — a pane's keys are {}",
                key_list(false)
            );
        };
        record(&mut seen, k, &at)?;
        only_a_value(child, k, &at)?;
        match k.set {
            Set::Text(set) => set(&mut decl, one_text(child, k, &at, load)?),
            Set::Count(set) => set(&mut decl, one_count(child, k, &at)?, &ctx)?,
            Set::Flag(set) => set(&mut decl, one_flag(child, k, &at)?),
            Set::List(set) => set(&mut decl, many_text(child, k, &at, load)?, &ctx)?,
            Set::FlagOrText(set) => set(&mut decl, one_mode(child, k, &at, load)?),
        }
    }
    Ok(decl)
}

/// `shell` is read before the pass that assigns it, because the command
/// split depends on it. A peek only: if `shell` is written in both
/// positions the pass raises the duplicate error, so the peek's choice
/// never reaches a spawn.
fn peek_shell(node: &kdl::KdlNode, at: &str) -> anyhow::Result<Option<ShellDecl>> {
    let k = key("shell").expect("`shell` is a pane key");
    if let Some(entry) = node.entry("shell") {
        return shell_decl(entry)
            .map(Some)
            .ok_or_else(|| prop_shape_err(k, at));
    }
    match node.children().and_then(|doc| doc.get("shell")) {
        Some(child) => match positional_entries(child).as_slice() {
            [entry] => shell_decl(entry).map(Some).ok_or_else(|| shape_err(k, at)),
            _ => Err(shape_err(k, at)),
        },
        None => Ok(None),
    }
}

/// What a string site needs beyond its own bytes: the declared
/// variables to hold its references to, the document text an error is
/// placed into, and the caller's color verdict (never an env sniff —
/// `--color` and `NO_COLOR` keep their one authority).
struct Load<'a> {
    text: &'a str,
    colored: bool,
    vars: &'a VariableBlock,
}

fn prop_text(
    entry: &kdl::KdlEntry,
    k: &Key,
    at: &str,
    load: &Load<'_>,
) -> anyhow::Result<Template> {
    let text = entry
        .value()
        .as_string()
        .ok_or_else(|| prop_shape_err(k, at))?;
    template_of(text, entry, load)
}

fn prop_count(value: &kdl::KdlValue, k: &Key, at: &str) -> anyhow::Result<i128> {
    value.as_integer().ok_or_else(|| prop_shape_err(k, at))
}

fn prop_flag(value: &kdl::KdlValue, k: &Key, at: &str) -> anyhow::Result<bool> {
    value.as_bool().ok_or_else(|| prop_shape_err(k, at))
}

/// Every positional entry of a node, with the ENTRY kept. `positional`
/// discards it by mapping `kdl::KdlEntry::value`, and a discarded
/// entry costs both the span an error points at and the string flavor
/// the raw-string rule reads (INV-1). `variables_block` needs both;
/// the other extraction sites migrate to it as they need it.
fn positional_entries(node: &kdl::KdlNode) -> Vec<&kdl::KdlEntry> {
    node.entries()
        .iter()
        .filter(|entry| entry.name().is_none())
        .collect()
}

/// Every positional entry of a node — the values a key was written with.
fn positional(node: &kdl::KdlNode) -> Vec<&kdl::KdlValue> {
    positional_entries(node)
        .into_iter()
        .map(kdl::KdlEntry::value)
        .collect()
}

fn one_text(node: &kdl::KdlNode, k: &Key, at: &str, load: &Load<'_>) -> anyhow::Result<Template> {
    match positional_entries(node).as_slice() {
        [entry] => {
            let text = entry.value().as_string().ok_or_else(|| shape_err(k, at))?;
            template_of(text, entry, load)
        }
        _ => Err(shape_err(k, at)),
    }
}

/// Record a string and hold every name it references to the declared
/// set (INV-6). The ONE place validation happens for pane and
/// `defaults` values, which is what makes a new string-valued key
/// inherit both for free — the point of chokepointing here.
fn template_of(text: &str, entry: &kdl::KdlEntry, load: &Load<'_>) -> anyhow::Result<Template> {
    // The flavor pick (INV-1): double quotes interpolate, single
    // quotes (KDL: raw strings) don't. Decided ONCE, at extraction, so
    // validation, the site rule, expansion, and the word split all
    // inherit it from the record without a second check anywhere.
    let template = if is_raw(entry) {
        Template::literal(text)
    } else {
        Template::extract(text)
    };
    validate_refs(template.refs(), entry, load)?;
    Ok(template)
}

/// Hold a recorded string's references to the declared set (INV-6).
/// Split out of `template_of` because `shell`'s dialect name needs the
/// same check without being a `PaneDecl` string field — one rule, two
/// callers, rather than two spellings that drift.
fn validate_refs(refs: &[String], entry: &kdl::KdlEntry, load: &Load<'_>) -> anyhow::Result<()> {
    for name in refs {
        if !load.vars.contains(name) {
            let at = reference_offset(load.text, entry, name);
            return Err(unknown_variable_err(
                load.text,
                &(at..at + name.len() + 4),
                name,
                load.vars,
                load.colored,
            ));
        }
    }
    Ok(())
}

/// Where a reference sits in the source: the entry's own span
/// (`KdlEntry::span()`, populated by `KdlDocument::parse` and starting
/// AFTER leading trivia), walked to the value's verbatim repr
/// (`KdlEntryFormat::value_repr`) and then to the `{{name}}` inside
/// it.
///
/// Every step is a `find` that may miss — an escaped spelling, an
/// absent `format()` on a programmatically built entry — and each miss
/// falls back to the enclosing span's start. A COARSER point, never a
/// wrong one: a column that points at the wrong byte is worse than one
/// that points at the value.
fn reference_offset(text: &str, entry: &kdl::KdlEntry, name: &str) -> usize {
    let start = entry.span().offset();
    let Some(source) = text.get(start..start + entry.span().len()) else {
        return start;
    };
    let Some(repr) = entry.format().map(|f| f.value_repr.as_str()) else {
        return start;
    };
    let Some(value_at) = source.find(repr) else {
        return start;
    };
    start + value_at + repr.find(&format!("{{{{{name}}}}}")).unwrap_or(0)
}

fn one_count(node: &kdl::KdlNode, k: &Key, at: &str) -> anyhow::Result<i128> {
    match positional(node).as_slice() {
        [value] => value.as_integer().ok_or_else(|| shape_err(k, at)),
        _ => Err(shape_err(k, at)),
    }
}

fn one_flag(node: &kdl::KdlNode, k: &Key, at: &str) -> anyhow::Result<bool> {
    match positional(node).as_slice() {
        [value] => value.as_bool().ok_or_else(|| shape_err(k, at)),
        _ => Err(shape_err(k, at)),
    }
}

/// A pane's `shell`, property spelling: the crate's one shell reader
/// (`shell_decl`) plus what a PANE's shell needs on top of it —
/// validation of a templated dialect name against the declared set.
fn prop_mode(
    entry: &kdl::KdlEntry,
    k: &Key,
    at: &str,
    load: &Load<'_>,
) -> anyhow::Result<ShellDecl> {
    let decl = shell_decl(entry).ok_or_else(|| prop_shape_err(k, at))?;
    validate_refs(decl.refs(), entry, load)?;
    Ok(decl)
}

fn one_mode(node: &kdl::KdlNode, k: &Key, at: &str, load: &Load<'_>) -> anyhow::Result<ShellDecl> {
    match positional_entries(node).as_slice() {
        [entry] => {
            let decl = shell_decl(entry).ok_or_else(|| shape_err(k, at))?;
            validate_refs(decl.refs(), entry, load)?;
            Ok(decl)
        }
        _ => Err(shape_err(k, at)),
    }
}

fn many_text(
    node: &kdl::KdlNode,
    k: &Key,
    at: &str,
    load: &Load<'_>,
) -> anyhow::Result<Vec<Template>> {
    let entries = positional_entries(node);
    if entries.is_empty() {
        return Err(shape_err(k, at));
    }
    entries
        .into_iter()
        .map(|entry| {
            let text = entry.value().as_string().ok_or_else(|| shape_err(k, at))?;
            template_of(text, entry, load)
        })
        .collect()
}

/// A bare positional that names a pane key — found only when a STRING
/// names one. A bare `#true` or `3` names nothing and keeps the
/// positional error it has today; a recognised name at ANY position
/// counts, because `defaults #true shell` still holds the key the
/// author was reaching for.
fn stray_key(values: &[&kdl::KdlValue]) -> Option<&'static Key> {
    values
        .iter()
        .find_map(|value| value.as_string().and_then(key))
}

/// The same scan for the document settings, which are not pane keys —
/// `gap` and `row-gap` are the whole dashboard's.
fn stray_setting(values: &[&kdl::KdlValue]) -> Option<&'static str> {
    values.iter().find_map(|value| {
        ["gap", "row-gap"]
            .into_iter()
            .find(|name| value.as_string() == Some(name))
    })
}

/// The teaching sentence for a pane key written bare in name position:
/// the property spelling, echoing the author's own value when one
/// follows the key so the shown fix is the line they meant to write,
/// the table's example otherwise. A List key has no property spelling,
/// so it gets the child-node sentence with the table's example — the
/// same one the property path teaches.
fn stray_key_err(at: &str, k: &Key, values: &[&kdl::KdlValue]) -> anyhow::Error {
    if let Set::List(_) = k.set {
        return anyhow!(
            "{at}: `{}` holds a list, so it must be a child node — write `{}` inside the block",
            k.name,
            k.example
        );
    }
    // A taught spelling must be WRITABLE: the author's value is echoed
    // only when it fits the key's shape — `pane "a" shell height` must
    // not teach `shell="height"`.
    let fits = |value: &kdl::KdlValue| match k.set {
        Set::Flag(_) => value.as_bool().is_some(),
        Set::Count(_) => value.as_integer().is_some(),
        Set::Text(_) => value.as_string().is_some(),
        Set::List(_) => false,
        // A value that names ANOTHER key is the next stray, not this
        // key's choice — echoing it would teach `shell="height"`.
        Set::FlagOrText(_) => match value.as_bool() {
            Some(_) => true,
            None => value
                .as_string()
                .filter(|name| !name.trim().is_empty())
                .is_some_and(|name| key(name).is_none()),
        },
    };
    let spelling = values
        .iter()
        .position(|value| value.as_string() == Some(k.name))
        .and_then(|i| values.get(i + 1))
        .filter(|value| fits(value))
        .map(|value| format!("{}={}", k.name, as_written(value)))
        .unwrap_or_else(|| k.property_example());
    anyhow!(
        "{at}: `{}` is a key, not an id — write `{spelling}`",
        k.name
    )
}

/// The teaching sentence for a document setting written on a block,
/// echoing the author's own value when one follows the name.
fn stray_setting_err(at: &str, name: &str, values: &[&kdl::KdlValue]) -> anyhow::Error {
    // Same writability rule as the key spelling: a setting takes an
    // integer, so anything else is never echoed into the example.
    let example = values
        .iter()
        .position(|value| value.as_string() == Some(name))
        .and_then(|i| values.get(i + 1))
        .filter(|value| value.as_integer().is_some())
        .map(|value| format!("{name} {}", as_written(value)))
        .unwrap_or_else(|| format!("{name} 1"));
    anyhow!(
        "{at}: `{name}` is the whole dashboard's, declared once at the top level as `{example}`"
    )
}

/// The container's own name — `a row` / `a column` — for the errors that
/// say what it holds.
fn container_kind(node: &kdl::KdlNode) -> &'static str {
    match node.name().value() {
        "column" => "a column",
        _ => "a row",
    }
}

/// A container holds cells, never keys. `gap` gets its own answer
/// because asking a row for a gap is a reasonable thing to try, and a
/// per-row gap is a decision nobody has made yet.
fn refuse_container_properties(node: &kdl::KdlNode, label: &str, kind: &str) -> anyhow::Result<()> {
    let Some(entry) = node.entries().iter().find(|entry| entry.name().is_some()) else {
        return Ok(());
    };
    let prop = entry.name().expect("filtered to properties").value();
    if prop == "gap" || prop == "row-gap" {
        bail!(
            "{label}: {kind} takes no properties — `{prop}` is the whole dashboard's, declared once at the top level as `{prop} 1`"
        );
    }
    bail!(
        "{label}: {kind} takes no properties, but {prop:?} is set — {kind} holds only `pane`, `row`, and `column` blocks"
    )
}

/// A user's token, quoted the way the rest of the catalog quotes them.
/// KDL v2 lets a string be written bare, and `KdlValue`'s own rendering
/// takes that shortest form — which turns `pane "a" "b"` into an error
/// about `b`, a token the file does not contain.
fn as_written(value: &kdl::KdlValue) -> String {
    match value.as_string() {
        Some(text) => format!("{text:?}"),
        None => value.to_string().trim().to_string(),
    }
}

/// A pane's id: exactly one string, unannotated. It is not an
/// internal handle — it renders as the box title and reaches the child
/// as `RAT_PANE` — so a second name or a number cannot be dropped.
fn one_id(node: &kdl::KdlNode, label: &str) -> anyhow::Result<String> {
    // Either position: `(u8)pane "log"` and `pane (name)"log"` are both
    // a token that reaches nothing (D-7).
    let annotation = std::iter::once(node.ty())
        .chain(
            node.entries()
                .iter()
                .filter(|entry| entry.name().is_none())
                .map(kdl::KdlEntry::ty),
        )
        .flatten()
        .next();
    if let Some(ty) = annotation {
        bail!(
            "{label}: the ({}) type annotation on a pane has no meaning here — remove it",
            ty.value()
        );
    }
    let values = positional(node);
    match values.as_slice() {
        [value] => {
            let id = value.as_string().map(str::to_string).ok_or_else(|| {
                anyhow!("{label}: a pane's id is a string — write `pane \"log\" {{ … }}`")
            })?;
            // RFC 3986 unreserved, one or more: every id is literally
            // a valid URI fragment, so `ref="#id"` never needs
            // percent-encoding. Display text belongs in `title`.
            if id.is_empty()
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
            {
                bail!(
                    "{label}: a pane's id sticks to letters, digits, and - . _ ~ — \
                     display text belongs in `title`"
                );
            }
            Ok(id)
        }
        [first, second, ..] => {
            // The scan starts at index 1: index 0 is the id slot,
            // always. KNOWN LIMIT, kept deliberately: `pane shell
            // #true` (the name omitted AND the bare spelling) keeps
            // the name error, because a stray key at index 0 cannot
            // be told apart from a pane genuinely named "shell" —
            // both defects are real, and the name error surfaces
            // first.
            if let Some(k) = stray_key(&values[1..]) {
                return Err(stray_key_err(label, k, &values[1..]));
            }
            bail!(
                "{label}: a pane takes ONE id, but {} follows {}",
                as_written(second),
                as_written(first)
            )
        }
        [] => bail!("{label}: this pane needs an id — write `pane \"log\" {{ … }}`"),
    }
}

/// Checked, field-named conversion: a negative value must FAIL LOUDLY,
/// never wrap — `as usize` would turn `gap -1` into a repeat count near
/// usize::MAX, which `" ".repeat(gap)` would try to allocate.
/// The `title` setting: an optional text, an optional `ref="#id"`,
/// at least one of the two. Same shape checks as the other document
/// settings — no annotation anywhere, no block — plus the reference
/// rules: a ref is a URI FRAGMENT, so a bare string is refused (the
/// whole non-`#` value space stays reserved for URI-references), and
/// the empty fragment (the document itself, per RFC 3986) is refused.
fn title_field(
    node: &kdl::KdlNode,
    load: &Load<'_>,
) -> anyhow::Result<crate::core::dashboard_file::TitleDecl> {
    if let Some(ty) = node.ty() {
        bail!(
            "the ({}) type annotation on `title` has no meaning here — remove it",
            ty.value()
        );
    }
    let mut reference = None;
    for entry in node.entries() {
        if let Some(ty) = entry.ty() {
            bail!(
                "the ({}) type annotation on `title` has no meaning here — remove it",
                ty.value()
            );
        }
        let Some(prop) = entry.name() else { continue };
        if prop.value() != "ref" {
            bail!(
                "title's one property is `ref` — write `title \"Deploy status\"` or `title ref=\"#header\"`"
            );
        }
        let Some(value) = entry.value().as_string() else {
            bail!("title's ref takes one string — write `ref=\"#header\"`");
        };
        let Some(fragment) = value.strip_prefix('#') else {
            bail!("title's ref is a URI fragment — write `ref=\"#header\"`");
        };
        if fragment.is_empty() {
            bail!("title ref \"#\" is the whole document — name a pane id, like `ref=\"#header\"`");
        }
        // A pane id is IDENTITY (INV-3), so a reference here is
        // refused rather than expanded — a computed id would make
        // `ref` unresolvable by reading the file.
        if !Template::extract(fragment).refs().is_empty() {
            bail!(
                "title ref \"#{fragment}\": a pane's id is not substitutable — it is the \
                 `RAT_PANE` value, the anchor a `ref` binds to, and a URI fragment. \
                 Display text belongs in `title`, which IS substitutable"
            );
        }
        if reference.replace(fragment.to_string()).is_some() {
            bail!("`title` is declared twice — a dashboard declares it once");
        }
    }
    if node.children().is_some() {
        bail!("title holds no block — write `title \"Deploy status\"` or `title ref=\"#header\"`");
    }
    let text = match positional_entries(node).as_slice() {
        [] => None,
        [entry] => {
            let text = entry.value().as_string().ok_or_else(|| {
                anyhow!("title takes one string — write `title \"Deploy status\"`")
            })?;
            Some(template_of(text, entry, load)?)
        }
        _ => bail!("title takes one string — write `title \"Deploy status\"`"),
    };
    if text.is_none() && reference.is_none() {
        bail!(
            "title takes a text, a ref=\"#id\", or both — write `title \"Deploy status\"` or `title ref=\"#header\"`"
        );
    }
    Ok(crate::core::dashboard_file::TitleDecl { text, reference })
}

fn usize_field(node: &kdl::KdlNode, name: &str) -> anyhow::Result<usize> {
    // A document setting's own answers: it is a node, not a key, so it
    // has no property spelling to offer — and like any key it carries
    // its value and nothing else.
    if let Some(ty) = node.ty() {
        bail!(
            "the ({}) type annotation on `{name}` has no meaning here — remove it",
            ty.value()
        );
    }
    if node.entries().iter().any(|entry| entry.name().is_some()) {
        bail!("{name} takes no properties — write `{name} 1`");
    }
    if let Some(entry) = node
        .entries()
        .iter()
        .find(|entry| entry.name().is_none() && entry.ty().is_some())
    {
        bail!(
            "the ({}) type annotation on `{name}` has no meaning here — remove it",
            entry.ty().expect("filtered to annotated").value()
        );
    }
    if node.children().is_some() {
        bail!("{name} takes one integer and holds no block — write `{name} 1`");
    }
    let cells = match positional(node).as_slice() {
        [value] => value
            .as_integer()
            .ok_or_else(|| anyhow!("{name} takes one integer — write `{name} 1`"))?,
        _ => bail!("{name} takes one integer — write `{name} 1`"),
    };
    usize::try_from(cells).map_err(|_| anyhow!("{name} must be a non-negative integer"))
}

/// The `variables` block: every declared variable, checked (INV-3,
/// INV-9) but never expanded (INV-2). A variable's name obeys the SAME
/// grammar a reference does (INV-1), so a name that could never be
/// written as `{{name}}` is refused where it is declared rather than
/// where it fails to resolve.
fn variables_block(
    node: &kdl::KdlNode,
    text: &str,
    colored: bool,
) -> anyhow::Result<VariableBlock> {
    refuse_annotation(node.ty(), "variables", "variables")?;
    if !positional(node).is_empty() {
        bail!("`variables` takes no id — it holds the board's variables, one per line");
    }
    let children = node
        .children()
        .map(kdl::KdlDocument::nodes)
        .unwrap_or_default();
    if children.is_empty() {
        bail!(
            "the `variables` block is empty — declare at least one variable, \
             like `plan \"/path/to/thing\"`, or drop the block"
        );
    }
    let mut vars: Vec<Variable> = Vec::with_capacity(children.len());
    for child in children {
        let name = child.name().value();
        let at = format!("variable {name:?}");
        if !is_reference_name(name) {
            bail!(
                "{at}: a variable's name starts with a letter or `_` and continues \
                 with letters, digits, `_` or `-` — the same spelling `{{{{{name}}}}}` uses"
            );
        }
        if vars.iter().any(|v| v.name == name) {
            bail!("{at}: `{name}` is declared twice — declare each variable once");
        }
        refuse_annotation(child.ty(), name, &at)?;
        if child.children().is_some() {
            bail!("{at}: `{name}` takes one string and holds no block — write `{name} \"value\"`");
        }
        let source = variable_source(child, name, &at)?;
        let entry = match positional_entries(child).as_slice() {
            [entry] if entry.value().as_string().is_some() => *entry,
            _ => bail!("{at}: `{name}` takes one string — write `{name} \"value\"`"),
        };
        vars.push(Variable {
            name: name.to_string(),
            source,
            // The flavor pick (INV-1): a raw variable value is literal
            // end to end and therefore references nothing — it forms
            // no graph edge and constrains no ordering.
            text: {
                let text = entry.value().as_string().expect("filtered to strings");
                if is_raw(entry) {
                    Template::literal(text)
                } else {
                    Template::extract(text)
                }
            },
            tier: Tier::Load, // replaced by `classify`
            span: span_range(entry),
        });
    }
    classify(vars, text, colored)
}

/// `shell` and `defer` are a variable's only properties. Each refusal
/// here is a DEAD KNOB refused in the house style — the same argument
/// that deleted `default`: a knob that can never fire is worse than
/// one that does not exist, because it reads as live.
fn variable_source(node: &kdl::KdlNode, name: &str, at: &str) -> anyhow::Result<VarSource> {
    let mut shell: Option<ShellDecl> = None;
    let mut defer = false;
    for entry in node.entries() {
        let Some(prop) = entry.name() else { continue };
        refuse_annotation(entry.ty(), prop.value(), at)?;
        match prop.value() {
            "shell" => {
                let mode = shell_decl(entry).ok_or_else(|| {
                    anyhow!("{at}: `shell` takes #true or one string — write `shell=#true`")
                })?;
                if !mode.runs_a_shell() {
                    bail!(
                        "{at}: `shell=#false` means no shell at all, and a variable with no \
                         shell is a constant — drop `shell` to make `{name}` a constant, or \
                         name the shell that runs it"
                    );
                }
                if shell.replace(mode).is_some() {
                    bail!("{at}: `shell` is declared twice — declare it once");
                }
            }
            "defer" => {
                defer = entry.value().as_bool().ok_or_else(|| {
                    anyhow!("{at}: `defer` takes #true or #false — write `defer=#true`")
                })?;
            }
            other => bail!(
                "{at}: unknown property {other:?} — a variable's properties are shell, defer{}",
                if other == "default" {
                    format!(
                        " — a constant is its own default: write `{name} \"50\"` and \
                         override it with `-v {name}=200`"
                    )
                } else {
                    String::new()
                }
            ),
        }
    }
    Ok(match (shell, defer) {
        (None, false) => VarSource::Constant,
        (None, true) => bail!(
            "{at}: `defer` re-derives a value, and `{name}` derives nothing — it is a \
             constant, the same bytes at every spawn. Drop `defer`, or give `{name}` a \
             `shell` command to re-run"
        ),
        (Some(mode), false) => VarSource::LoadCommand(mode),
        (Some(mode), true) => VarSource::SpawnCommand(mode),
    })
}

/// A `shell` entry as a [`ShellDecl`] — the template-carrying reader.
/// `#true` is the platform's shell, `#false` no shell at all, and a
/// string names the program. An EMPTY string names nothing: `None`
/// here, a shape error at the call site, never a spawn of `""`.
///
/// It takes the ENTRY, not the value, because a dialect name is a
/// string an author writes: it carries references, and whether they
/// ARE references depends on the string's flavor (INV-1). Validating
/// those references is the CALLER's job, because the callers answer it
/// differently: a pane's shell validates against the declared set,
/// while a variable's shell becomes a graph edge and is answered by
/// `classify`.
///
/// Private, deliberately: every caller is in this module. It takes a
/// `kdl::KdlEntry`, so making it public would export a KDL-shaped
/// parser out of the walk. If a future caller outside this module
/// genuinely needs to PARSE a shell entry, widen it to `pub(crate)`
/// and say why here — not to `pub`.
/// Is this entry's value a RAW KDL string? Decided lexically from the
/// value's verbatim source text, because the parsed value cannot say:
/// the v2 `KdlValue` has one `String` variant and the v1 bridge
/// collapses raw into it. `KdlEntryFormat::value_repr` holds the
/// source bytes, and a raw string opens with one or more `#` then a
/// quote — `#"`, `##"`, `#"""`, at any depth.
///
/// The rule this decides (INV-1), in the analogy every reader already
/// owns: **double quotes interpolate, single quotes don't.** A normal
/// string interpolates `{{name}}`; a raw string is literal end to
/// end, and `{{` is not even recognized there.
///
/// The known cost, recorded because it is a real collision: one
/// literal cannot be both raw (backslash freedom) and interpolating.
/// A `script` body that wants awk/sed backslashes AND `{{name}}` must
/// use a normal string and escape its backslashes, or read the value
/// from the environment instead.
///
/// `format()` is `None` for a programmatically built entry and after
/// `clear_format()`/`autoformat()`; ratto only ever parses from
/// source, so that never happens here. The fallback is **normal**, and
/// deliberately so: defaulting to raw would SILENTLY stop
/// interpolating a value the author wrote to interpolate — a wrong
/// answer with no error — while defaulting to normal can at worst
/// produce a loud unknown-variable refusal.
fn is_raw(entry: &kdl::KdlEntry) -> bool {
    let Some(repr) = entry.format().map(|f| f.value_repr.trim_start()) else {
        return false;
    };
    let after_hashes = repr.trim_start_matches('#');
    after_hashes.len() < repr.len() && after_hashes.starts_with('"')
}

fn shell_decl(entry: &kdl::KdlEntry) -> Option<ShellDecl> {
    if let Some(flag) = entry.value().as_bool() {
        return Some(if flag {
            ShellDecl::Platform
        } else {
            ShellDecl::Direct
        });
    }
    entry
        .value()
        .as_string()
        .filter(|name| !name.trim().is_empty())
        // The flavor pick (INV-1), and the highest-stakes of the three
        // sites that make it: a dialect name's references are graph
        // EDGES (`Variable::refs`, in `variables.rs`) and tier-analysis
        // participants, so a raw name that wrongly recorded one could
        // refuse a board for a cycle or a tier violation INV-1 says
        // cannot exist.
        .map(|name| {
            ShellDecl::Named(if is_raw(entry) {
                Template::literal(name)
            } else {
                Template::extract(name)
            })
        })
}

/// Build the dependency graph, refuse cycles by NAME PATH, order
/// dependencies first, and classify every effective tier — one
/// post-order DFS, because a variable's referents are classified
/// before it is, which is what topological order means (INV-3, INV-9).
///
/// Roots are visited in DECLARATION order so a board with two
/// independent cycles always names the same one; nothing else here
/// reads position.
fn classify(mut vars: Vec<Variable>, text: &str, colored: bool) -> anyhow::Result<VariableBlock> {
    // The adjacency list is built FIRST, and every unknown name is
    // refused here — so the DFS walks indices only and never has to
    // borrow `vars` while mutating it.
    let mut deps: Vec<Vec<usize>> = Vec::with_capacity(vars.len());
    for var in &vars {
        let mut edges = Vec::new();
        // `Variable::refs()`, not the text's refs alone: a templated
        // `shell` dialect name is a dependency too.
        for name in var.refs() {
            match vars.iter().position(|other| other.name == name) {
                Some(i) if !edges.contains(&i) => edges.push(i),
                Some(_) => {}
                None => {
                    let declared = VariableBlock::new(vars.clone(), Vec::new());
                    return Err(unknown_variable_err(
                        text, &var.span, name, &declared, colored,
                    ));
                }
            }
        }
        deps.push(edges);
    }

    #[derive(Copy, Clone, PartialEq)]
    enum Mark {
        White,
        Gray,
        Black,
    }
    let mut mark = vec![Mark::White; vars.len()];
    let mut order: Vec<usize> = Vec::with_capacity(vars.len());
    for root in 0..vars.len() {
        if mark[root] != Mark::White {
            continue;
        }
        // An explicit stack of (index, next-edge cursor) rather than
        // recursion: a board is small, but a parser that can be made
        // to blow the stack by a deep file is a parser with a denial
        // of service in it.
        let mut stack: Vec<(usize, usize)> = vec![(root, 0)];
        mark[root] = Mark::Gray;
        while let Some(top) = stack.len().checked_sub(1) {
            let (at, cursor) = stack[top];
            if let Some(&next) = deps[at].get(cursor) {
                stack[top].1 += 1;
                match mark[next] {
                    Mark::White => {
                        mark[next] = Mark::Gray;
                        stack.push((next, 0));
                    }
                    Mark::Gray => {
                        // A Gray referent closes a cycle: the path runs
                        // from its first appearance on the stack through
                        // the current variable and back to itself.
                        let start = stack
                            .iter()
                            .position(|&(i, _)| i == next)
                            .expect("a Gray variable is on the stack");
                        let path: Vec<&str> = stack[start..]
                            .iter()
                            .map(|&(i, _)| vars[i].name.as_str())
                            .chain(std::iter::once(vars[next].name.as_str()))
                            .collect();
                        return Err(cycle_err(text, &vars[next].span, &path, colored));
                    }
                    Mark::Black => {}
                }
            } else {
                // Post-order: every referent is Black, so its effective
                // tier is final (INV-9's topological reading).
                let declared = vars[at].source.declared_tier();
                let effective = deps[at]
                    .iter()
                    .map(|&d| vars[d].tier)
                    .fold(declared, Tier::max);
                if declared == Tier::Load && effective == Tier::Spawn {
                    let referent = deps[at]
                        .iter()
                        .find(|&&d| vars[d].tier == Tier::Spawn)
                        .map(|&d| vars[d].name.clone())
                        .expect("a Spawn-tier referent forced the promotion");
                    return Err(tier_violation_err(
                        text,
                        &vars[at].span,
                        &vars[at].name,
                        &referent,
                        colored,
                    ));
                }
                vars[at].tier = effective;
                mark[at] = Mark::Black;
                order.push(at);
                stack.pop();
            }
        }
    }
    Ok(VariableBlock::new(vars, order))
}

fn cycle_err(
    text: &str,
    span: &std::ops::Range<usize>,
    path: &[&str],
    colored: bool,
) -> anyhow::Error {
    placed_error(
        text,
        span,
        &format!(
            "variable cycle: {}",
            path.iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(" → ")
        ),
        Some("a variable cannot derive from itself, however many hops away"),
        colored,
    )
}

/// INV-6's unknown-variable error, defined HERE because the variables
/// block is its first consumer; the same function applies at every
/// other string site. The declared-set breadcrumb mirrors `key_list`,
/// and an empty block says so rather than printing an empty list.
pub(crate) fn unknown_variable_err(
    text: &str,
    span: &std::ops::Range<usize>,
    name: &str,
    declared: &VariableBlock,
    colored: bool,
) -> anyhow::Error {
    let help = if declared.is_empty() {
        "this board declares no variables — add a `variables` block, or write the value literally"
            .to_string()
    } else {
        format!("declared variables are {}", declared.declared_list())
    };
    placed_error(
        text,
        span,
        &format!("unknown variable `{name}`"),
        Some(&help),
        colored,
    )
}

fn tier_violation_err(
    text: &str,
    span: &std::ops::Range<usize>,
    who: &str,
    referent: &str,
    colored: bool,
) -> anyhow::Error {
    placed_error(
        text,
        span,
        &format!("`{who}` is not deferred but references `{referent}`, which is"),
        Some(&format!(
            "add `defer=#true` to `{who}`, or drop it from `{referent}`"
        )),
        colored,
    )
}

/// A semantic error, placed in the source the way a KDL syntax error
/// is: the greppable `line N, column M: …` head from
/// `syntax_error_text`, then miette's snippet block echoing the
/// offending line. `line_column` walks BYTES — kdl's own span doc says
/// chars and is wrong, which that function already records.
fn placed_error(
    text: &str,
    span: &std::ops::Range<usize>,
    message: &str,
    help: Option<&str>,
    colored: bool,
) -> anyhow::Error {
    use std::fmt::Write;
    let (line, column) = line_column(text, span.start);
    let mut out = syntax_error_text(line, column, Some(message));
    let diagnostic = kdl::KdlDiagnostic {
        input: std::sync::Arc::new(text.to_string()),
        span: (span.start, span.len()).into(),
        message: Some(message.to_string()),
        label: Some("here".to_string()),
        help: help.map(str::to_string),
        severity: miette::Severity::Error,
    };
    // Same theme choice and same fixed width as `syntax_error`: the
    // caller's stream decides, never an env sniff.
    let theme = if colored {
        miette::GraphicalTheme {
            characters: miette::ThemeCharacters::unicode(),
            styles: miette::ThemeStyles::ansi(),
        }
    } else {
        miette::GraphicalTheme::unicode_nocolor()
    };
    let handler = miette::GraphicalReportHandler::new_themed(theme).with_width(80);
    let mut block = String::new();
    if handler.render_report(&mut block, &diagnostic).is_ok() {
        let _ = write!(out, "\n{}", block.trim_end());
    }
    anyhow!("{out}")
}

/// An entry's value span as a byte range. `KdlEntry::span()` covers
/// the entry as written — `key=value` for a property, the bare value
/// for a positional — and starts AFTER leading trivia.
fn span_range(entry: &kdl::KdlEntry) -> std::ops::Range<usize> {
    let span = entry.span();
    span.offset()..span.offset() + span.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::registry::Registry;

    #[test]
    fn line_column_counts_from_one_over_bytes_not_chars() {
        assert_eq!(line_column("a\nbb\n", 0), (1, 1));
        assert_eq!(line_column("a\nbb\n", 2), (2, 1));
        assert_eq!(line_column("a\nbb\n", 4), (2, 3));
        // Multi-byte: 'é' is two bytes, one column. Offset 6 is the
        // space — the sixth CHAR — reached by walking BYTES. This is
        // the case that proves the convention (and that a mid-char
        // offset cannot panic the walk).
        assert_eq!(line_column("héllo x", 6), (1, 6));
        // A mid-char byte offset lands after the char it is inside,
        // and never panics: byte 2 is é's second byte, and é is
        // consumed whole, so the walk stops at column 3.
        assert_eq!(line_column("héllo x", 2), (1, 3));
        // CRLF: the \r is not a column.
        assert_eq!(line_column("ab\r\ncd", 4), (2, 1));
        // Past the end: clamp to one past the last content.
        assert_eq!(line_column("ab\n", 99), (2, 1));
    }

    #[test]
    fn a_bare_key_is_found_only_when_it_names_one() {
        use kdl::KdlValue;
        let shell = KdlValue::String("shell".into());
        let nothing = KdlValue::String("frobnicate".into());
        let not_a_string = KdlValue::Bool(true);
        let gap = KdlValue::String("gap".into());
        assert_eq!(stray_key(&[&shell]).map(|k| k.name), Some("shell"));
        assert!(stray_key(&[&nothing]).is_none());
        assert!(stray_key(&[&not_a_string]).is_none());
        // The SECOND positional counts too: `defaults #true shell`
        // still holds a recognisable key.
        assert_eq!(
            stray_key(&[&not_a_string, &shell]).map(|k| k.name),
            Some("shell")
        );
        assert_eq!(stray_setting(&[&gap]), Some("gap"));
        assert!(stray_setting(&[&shell]).is_none());
    }

    #[test]
    fn a_bare_key_on_defaults_teaches_the_property_spelling() {
        assert_eq!(
            container_err("defaults shell #true\npane \"a\" { command \"true\" height 3 }"),
            "defaults: `shell` is a key, not an id — write `shell=#true`"
        );
        assert_eq!(
            container_err("defaults interval \"5s\"\npane \"a\" { command \"true\" height 3 }"),
            "defaults: `interval` is a key, not an id — write `interval=\"5s\"`"
        );
        // A List key has no property spelling — the child-node
        // sentence, the same one the property path teaches.
        assert_eq!(
            container_err(
                "defaults command \"git\" \"log\"\npane \"a\" { command \"true\" height 3 }"
            ),
            "defaults: `command` holds a list, so it must be a child node — write `command \"git\" \"log\"` inside the block"
        );
        // gap is the dashboard's, not a pane key.
        assert_eq!(
            container_err("defaults gap 1\npane \"a\" { command \"true\" height 3 }"),
            "defaults: `gap` is the whole dashboard's, declared once at the top level as `gap 1`"
        );
    }

    #[test]
    fn a_bare_key_after_a_pane_name_teaches_the_property_spelling() {
        assert_eq!(
            container_err("pane \"a\" shell #true { command \"true\" height 3 }"),
            "pane #1: `shell` is a key, not an id — write `shell=#true`"
        );
        // The breadcrumb survives nesting, and the spelling echoes the
        // author's own value.
        assert_eq!(
            container_err("row {\n    pane \"a\" height 3 { command \"true\" }\n}"),
            "row #1 > pane #1: `height` is a key, not an id — write `height=3`"
        );
        assert_eq!(
            container_err("pane \"a\" command \"git\" \"log\" { height 3 }"),
            "pane #1: `command` holds a list, so it must be a child node — write `command \"git\" \"log\"` inside the block"
        );
    }

    #[test]
    fn an_echoed_value_that_does_not_fit_the_key_falls_back_to_the_example() {
        // A taught spelling must be WRITABLE. When the value after the
        // stray key does not fit the key's shape — here a second key
        // name where a boolean belongs — echoing it would teach
        // `shell="height"`, an error of its own. The table's example
        // is the fallback.
        assert_eq!(
            container_err("pane \"a\" shell height 3 { command \"true\" }"),
            "pane #1: `shell` is a key, not an id — write `shell=#true`"
        );
        // A string that names no other key DOES fit `shell` now, so
        // the author's own shell name is the taught spelling.
        assert_eq!(
            container_err("pane \"a\" shell fish { command \"true\" }"),
            "pane #1: `shell` is a key, not an id — write `shell=\"fish\"`"
        );
        assert_eq!(
            container_err("defaults height #true\npane \"a\" { command \"true\" height 3 }"),
            "defaults: `height` is a key, not an id — write `height=7`"
        );
        // The same rule for a document setting: `gap` takes an
        // integer, so a non-integer is never echoed into the example.
        assert_eq!(
            container_err("row gap #true { pane \"a\" height 3 { command \"true\" } }"),
            "row #1: `gap` is the whole dashboard's, declared once at the top level as `gap 1`"
        );
    }

    #[test]
    fn a_bare_gap_on_a_container_names_the_dashboards_gap() {
        assert_eq!(
            container_err("row gap 1 { pane \"a\" height 3 { command \"true\" } }"),
            "row #1: `gap` is the whole dashboard's, declared once at the top level as `gap 1`"
        );
        // The other container, the other setting, the author's own
        // value — one template, every substitution pinned.
        assert_eq!(
            container_err("column row-gap 2 { pane \"a\" height 3 { command \"true\" } }"),
            "column #1: `row-gap` is the whole dashboard's, declared once at the top level as `row-gap 2`"
        );
    }

    #[test]
    fn a_bare_pane_key_on_a_container_says_where_it_belongs() {
        assert_eq!(
            container_err("row shell #true { pane \"a\" height 3 { command \"true\" } }"),
            "row #1: `shell` is a pane's key — write it on a `pane` block inside this row"
        );
    }

    #[test]
    fn a_colored_parse_paints_the_snippet_and_the_plain_one_does_not() {
        let bad = "pane \"log\" interval=5s {\n    command \"date\"\n}\n";
        let colored = format!("{:#}", parse_styled(bad, true).expect_err("still invalid"));
        let plain = format!("{:#}", parse_styled(bad, false).expect_err("still invalid"));
        assert!(colored.contains('\u{1b}'), "got {colored:?}");
        assert!(
            colored.starts_with("line 1, column "),
            "color never touches the greppable head: {colored:?}"
        );
        assert!(!plain.contains('\u{1b}'), "got {plain:?}");
        assert_eq!(
            plain,
            format!("{:#}", parse(bad).expect_err("still invalid")),
            "`parse` IS the plain spelling"
        );
    }

    #[test]
    fn a_kdl_syntax_error_carries_its_line_and_column() {
        // The reported repro: a bare token in property position is
        // not a KDL value, and the answer must say where — AND show
        // the offending line itself, rustc-style. The snippet's exact
        // glyphs are miette's and stay unpinned; the SOURCE ECHO is
        // the contract.
        let err = parse("pane \"log\" interval=5s {\n    command \"date\"\n}\n")
            .expect_err("5s is not a valid KDL value");
        let text = format!("{err:#}");
        assert!(text.starts_with("line 1, column "), "got {text}");
        assert!(!text.contains("Failed to parse KDL document"), "got {text}");
        assert!(
            text.contains("pane \"log\" interval=5s {"),
            "the offending source line is echoed: {text}"
        );
    }

    #[test]
    fn an_error_with_no_diagnostics_keeps_the_crates_own_sentence() {
        // The fallback is the crate's Display, compared at runtime —
        // upstream's bytes are not ours to freeze.
        let err = kdl::KdlError {
            input: std::sync::Arc::new(String::new()),
            diagnostics: Vec::new(),
        };
        assert_eq!(
            format!("{}", syntax_error("", &err, false)),
            format!("{err}")
        );
    }

    #[test]
    fn the_earliest_diagnostic_heads_and_every_diagnostic_gets_a_block() {
        // Two bad values on two lines: the first failure heads the
        // message, and each diagnostic renders its own snippet block
        // below — no counting, no hiding. The `[line:column]` span
        // marker is the loosest stable probe of miette's block
        // header; revisit if a miette bump reformats it.
        let err = parse("a 1.\nb 2.\n").expect_err("both floats are invalid");
        let text = format!("{err:#}");
        assert!(text.starts_with("line 1, column "), "got {text}");
        assert!(text.contains("[1:3]"), "the first block is placed: {text}");
        assert!(text.contains("[2:3]"), "the second block is placed: {text}");
    }

    #[test]
    fn the_head_line_is_the_place_and_the_message() {
        // OUR frame is pinned whole; upstream's message text is a
        // plain value here, never byte-pinned at the route. Help and
        // the other diagnostics live in the snippet blocks now, so
        // the head is just the greppable place + message.
        assert_eq!(
            syntax_error_text(1, 20, Some("Expected valid value")),
            "line 1, column 20: Expected valid value"
        );
        assert_eq!(
            syntax_error_text(3, 1, Some("No closing '}' for child block")),
            "line 3, column 1: No closing '}' for child block"
        );
        assert_eq!(
            syntax_error_text(1, 1, None),
            "line 1, column 1: invalid KDL"
        );
    }

    const KDL_FIXTURE: &str = r#"
gap 1

defaults {
    interval "5s"
    border "rounded"
    padding "0 1"
    height 7
}

pane "clock" {
    command "date +%H:%M:%S"
    interval "60s"
    trigger "file:./stamp" "file:./notes"
    height 16
    width "2fr"
}

row {
    pane "branch" {
        command "git" "branch" "--show-current"
    }
    pane "notes" {
        command "rat style hello"
        interval "never"
    }
}
"#;

    #[test]
    fn the_fixture_parses_to_the_declared_dashboard() {
        // The thinness proof, one-sided since the TOML grammar's
        // deletion: the parser emits exactly what the file declares —
        // word splitting, defaults, layout shape — with no rule of its
        // own. (Its two-grammar ancestor also asserted TOML equality;
        // that property lost its second side with the format pick.)
        let from_kdl = parse(KDL_FIXTURE).expect("kdl parses");
        assert_eq!(from_kdl.gap, Some(1));
        assert_eq!(from_kdl.panes.len(), 3);
        assert_eq!(
            from_kdl.panes[0].command,
            Some(vec!["date".into(), "+%H:%M:%S".into()])
        );
        assert_eq!(
            from_kdl.panes[1].command,
            Some(vec!["git".into(), "branch".into(), "--show-current".into()])
        );
        assert_eq!(from_kdl.defaults.height, Some(7));
        use crate::core::dashboard_file::LayoutDecl;
        assert_eq!(
            from_kdl.layout,
            Some(vec![
                LayoutDecl::Pane("clock".to_string()),
                LayoutDecl::Row(vec![
                    LayoutDecl::Pane("branch".to_string()),
                    LayoutDecl::Pane("notes".to_string()),
                ]),
            ])
        );
    }

    // ---------------------------------------------------------------
    // The inline tree: a pane is declared inside the row or column that
    // places it.
    // ---------------------------------------------------------------

    const INLINE_THREE_PANE: &str = r#"
gap 1

defaults {
    interval "5s"
    border "rounded"
    padding "0 1"
    height 7
}

row {
    pane "log" {
        command "git" "log" "--oneline" "-3"
        interval "15s"
    }
    pane "branch" {
        command "git" "status" "--short" "--branch"
    }
}

row {
    pane "clock" {
        command "date" "+%H:%M:%S"
        interval "1s"
        height 4
    }
}
"#;

    const INLINE_NESTED: &str = r#"
gap 1

defaults {
    interval "5s"
    height 7
}

row {
    column {
        pane "log" {
            command "git" "log" "--oneline" "-3"
            interval "15s"
        }
        pane "branch" {
            command "git" "status" "--short" "--branch"
        }
    }
    column {
        pane "clock" {
            command "date" "+%H:%M:%S"
            interval "1s"
            height 4
        }
    }
}

pane "nested" {
    command "rat" "dashboard" "examples/panes.kdl" "--once"
    height 15
}
"#;

    /// The examples are the grammar most people will read first, so
    /// `include_str!` puts them in the suite: one that stops parsing
    /// fails a test instead of rotting quietly in the repository. (This
    /// is what survives of 3.1's characterization pins — they compared
    /// each example against the text it replaced, and that comparison
    /// lost its second side when the old parser went.)
    #[test]
    fn the_shipped_examples_declare_real_dashboards() {
        for text in [
            include_str!("../../examples/panes.kdl"),
            include_str!("../../examples/panes-nested.kdl"),
            include_str!("../../examples/follow.kdl"),
            include_str!("../../examples/script.kdl"),
        ] {
            parse(text)
                .expect("the example parses")
                .into_registry()
                .expect("the example validates");
        }
    }

    fn names_of(file: &DashboardFile) -> Vec<&str> {
        file.panes
            .iter()
            .map(|decl| decl.id.as_deref().expect("a named pane"))
            .collect()
    }

    /// `Registry` carries no `PartialEq` — it is compared the way the
    /// loop reads it: one source and one box per id, and the tree.
    fn assert_same_registry(left: &Registry, right: &Registry) {
        assert_eq!(left.len(), right.len());
        assert_eq!(left.composition(), right.composition());
        for id in left.ids() {
            assert_eq!(left.spec(id), right.spec(id));
            assert_eq!(left.pane(id), right.pane(id));
        }
    }

    #[test]
    fn an_inline_pane_declares_where_it_sits() {
        let inline = parse(INLINE_THREE_PANE).expect("the inline spelling parses");
        // The lift is visibly a MOVE, not a resolution: a pane written
        // inside its row lands in the flat declaration list, its name
        // lands in the tree, and every token is still exactly what the
        // file wrote.
        assert_eq!(names_of(&inline), ["log", "branch", "clock"]);
        assert_eq!(
            inline.panes[0].command,
            Some(vec![
                "git".into(),
                "log".into(),
                "--oneline".into(),
                "-3".into(),
            ])
        );
        assert_eq!(
            inline.panes[0].interval.as_ref().map(Template::as_str),
            Some("15s")
        );
        assert_eq!(inline.panes[2].height, Some(4));
        assert_eq!(inline.defaults.height, Some(7));
        assert_eq!(inline.gap, Some(1));
        assert_eq!(
            inline.layout,
            Some(vec![
                LayoutDecl::Row(vec![
                    LayoutDecl::Pane("log".to_string()),
                    LayoutDecl::Pane("branch".to_string()),
                ]),
                // The one-cell row collapses to its cell.
                LayoutDecl::Pane("clock".to_string()),
            ])
        );
    }

    #[test]
    fn the_inline_tree_nests_to_the_same_depth() {
        let inline = parse(INLINE_NESTED).expect("the inline spelling parses");
        // Document order is declaration order is `SourceId` order: the
        // walk is depth-first, so a pane's id is its reading position —
        // and the last pane here is a top-level one, after two levels of
        // nesting (D-2).
        assert_eq!(names_of(&inline), ["log", "branch", "clock", "nested"]);
        assert_eq!(inline.panes[3].height, Some(15));
        assert_eq!(
            inline.layout,
            Some(vec![
                LayoutDecl::Row(vec![
                    LayoutDecl::Column(vec![
                        LayoutDecl::Pane("log".to_string()),
                        LayoutDecl::Pane("branch".to_string()),
                    ]),
                    LayoutDecl::Pane("clock".to_string()),
                ]),
                LayoutDecl::Pane("nested".to_string()),
            ])
        );
    }

    #[test]
    fn a_top_level_pane_is_a_cell_in_the_dashboards_column() {
        // D-2: a pane needs no container to be placed. The top level IS
        // the dashboard's column, so a top-level pane is a full-width
        // cell in it — which is why a layout-less file stays a legal
        // file and the minimal dashboard stays one node.
        let file = parse(
            "pane \"a\" {\n    height 3\n    command \"date\"\n}\npane \"b\" {\n    height 3\n    command \"date\"\n}\npane \"c\" {\n    height 3\n    command \"date\"\n}\n",
        )
        .expect("flat panes parse");
        assert_eq!(
            file.layout,
            Some(vec![
                LayoutDecl::Pane("a".to_string()),
                LayoutDecl::Pane("b".to_string()),
                LayoutDecl::Pane("c".to_string()),
            ])
        );

        // …and that stated column is exactly what an ABSENT layout has
        // always resolved to, so nothing about those files changed.
        let implicit = DashboardFile {
            layout: None,
            ..file.clone()
        };
        assert_same_registry(
            &file.into_registry().expect("the stated column validates"),
            &implicit
                .into_registry()
                .expect("the implicit column validates"),
        );
    }

    /// A cell that is not a cell is located by tree position, because
    /// position is all a container knows about it — and often all the
    /// pane has, since it may not have named itself yet. These assert
    /// the WHOLE message: a teaching error that drifts a clause stops
    /// teaching, and there is no second copy of the text to compare to.
    fn container_err(text: &str) -> String {
        format!("{:#}", parse(text).unwrap_err())
    }

    #[test]
    fn an_inline_pane_needs_a_name_and_the_error_names_its_cell() {
        assert_eq!(
            container_err(
                "row {\n    pane \"log\" {\n        command \"date\"\n    }\n    pane {\n        command \"date\"\n    }\n}\n"
            ),
            "row #1 > pane #2: this pane needs an id — write `pane \"log\" { … }`"
        );
    }

    #[test]
    fn a_pane_takes_one_id() {
        assert_eq!(
            container_err("row {\n    pane \"a\" \"b\" {\n        command \"date\"\n    }\n}\n"),
            "row #1 > pane #1: a pane takes ONE id, but \"b\" follows \"a\""
        );
        assert_eq!(
            container_err("row {\n    pane 3 {\n        command \"date\"\n    }\n}\n"),
            "row #1 > pane #1: a pane's id is a string — write `pane \"log\" { … }`"
        );
    }

    /// With the flat list gone a bare name refers to nothing, so the
    /// error is a migration teacher: it shows the one spelling (D-3).
    #[test]
    fn a_row_holds_pane_blocks_not_pane_names() {
        assert_eq!(
            container_err("row \"log\"\n"),
            "row #1: a row holds `pane` blocks, not pane ids — declare the pane where it sits, like `row { pane \"log\" { … } }`"
        );
    }

    #[test]
    fn an_empty_container_says_to_put_a_pane_in_it() {
        assert_eq!(
            container_err("row {\n}\n"),
            "row #1: this row is empty — put at least one pane in it"
        );
        assert_eq!(
            container_err(
                "row {\n    pane \"a\" {\n        command \"date\"\n    }\n    column {\n    }\n}\n"
            ),
            "row #1 > column #2: this column is empty — put at least one pane in it"
        );
    }

    /// D-5: a per-row gap is a real future request, so `row gap=2` gets
    /// its own answer rather than a generic refusal — the feature stays
    /// an open decision instead of becoming a shipped lie.
    #[test]
    fn a_row_takes_no_properties() {
        assert_eq!(
            container_err(
                "row style=\"x\" {\n    pane \"a\" {\n        command \"date\"\n    }\n}\n"
            ),
            "row #1: a row takes no properties, but \"style\" is set — a row holds only `pane`, `row`, and `column` blocks"
        );
        assert_eq!(
            container_err("row gap=2 {\n    pane \"a\" {\n        command \"date\"\n    }\n}\n"),
            "row #1: a row takes no properties — `gap` is the whole dashboard's, declared once at the top level as `gap 1`"
        );
    }

    #[test]
    fn an_unknown_node_in_a_row_names_the_three_it_holds() {
        assert_eq!(
            container_err("row {\n    panel {\n        command \"date\"\n    }\n}\n"),
            "row #1 > panel #1: unknown node \"panel\" — a row holds `pane`, `row`, and `column` blocks"
        );
    }

    /// The deleted spelling teaches (I-57). A `layout` block was the
    /// whole of the old grammar's placement, so meeting one is not an
    /// unknown-token complaint — it is a reader who knows the old
    /// spelling, and the error owes them the new one.
    #[test]
    fn a_layout_block_says_there_is_none() {
        assert_eq!(
            container_err(
                "pane \"log\" {\n    command \"date\"\n}\nlayout {\n    row \"log\"\n}\n"
            ),
            "there is no `layout` block — a pane is declared inside the row or column that places it: write `row { pane \"log\" { … } pane \"branch\" { … } }`"
        );
    }

    #[test]
    fn an_unknown_top_level_node_names_the_eight() {
        assert_eq!(
            container_err("panes {\n    pane \"log\" {\n        command \"date\"\n    }\n}\n"),
            "unknown node \"panes\" — a dashboard's top level takes title, gap, row-gap, variables, defaults, pane, row, or column"
        );
    }

    /// D-1: `defaults` has no position in the geometry, so it lives at
    /// the top level beside the tree — inside a container it is just an
    /// unknown node.
    #[test]
    fn a_defaults_block_belongs_at_the_top_level() {
        assert_eq!(
            container_err("row {\n    defaults {\n        height 3\n    }\n}\n"),
            "row #1 > defaults #1: unknown node \"defaults\" — a row holds `pane`, `row`, and `column` blocks"
        );
    }

    #[test]
    fn a_single_cell_row_collapses_to_its_pane() {
        // Every top-level item is normalized, or a one-cell row would
        // declare a different tree than the pane it holds and every
        // equality above would fail.
        let file = parse("row {\n    pane \"clock\" {\n        command \"date\"\n    }\n}\n")
            .expect("a one-cell row parses");
        assert_eq!(
            file.layout,
            Some(vec![LayoutDecl::Pane("clock".to_string())])
        );
    }

    /// One table, thirteen keys: every key a pane accepts must land on
    /// the declaration through it. A key the table forgets shows up
    /// here as a `None` field, not as a silently ignored node.
    #[test]
    fn every_pane_key_reaches_the_declaration_through_one_table() {
        let file = parse(
            r#"
pane "all" {
    command "git" "log"
    shell #false
    interval "5s"
    trigger "file:./stamp" "file:./notes"
    trigger-debounce "250ms"
    height 7
    width "2fr"
    overflow "keep-bottom"
    border "rounded"
    padding "0 1"
    title "Recent commits"
    chrome #false
    focusable #false
}
"#,
        )
        .expect("parses");
        let pane = &file.panes[0];
        assert_eq!(pane.id.as_deref(), Some("all"));
        assert_eq!(pane.command, Some(vec!["git".into(), "log".into()]));
        assert_eq!(pane.shell, Some(ShellDecl::Direct));
        assert_eq!(pane.interval.as_ref().map(Template::as_str), Some("5s"));
        assert_eq!(
            pane.trigger,
            Some(vec!["file:./stamp".into(), "file:./notes".into()])
        );
        assert_eq!(
            pane.trigger_debounce.as_ref().map(Template::as_str),
            Some("250ms")
        );
        assert_eq!(pane.height, Some(7));
        assert_eq!(pane.width.as_ref().map(Template::as_str), Some("2fr"));
        assert_eq!(
            pane.overflow.as_ref().map(Template::as_str),
            Some("keep-bottom")
        );
        assert_eq!(pane.border.as_ref().map(Template::as_str), Some("rounded"));
        assert_eq!(pane.padding.as_ref().map(Template::as_str), Some("0 1"));
        assert_eq!(
            pane.title.as_ref().map(Template::as_str),
            Some("Recent commits")
        );
        assert_eq!(pane.chrome, Some(false));
        assert_eq!(pane.focusable, Some(false));
    }

    #[test]
    fn the_script_key_reaches_the_declaration_through_the_table() {
        // Parse-level only: `script` beside `command` is a VALIDATION
        // refusal (into_registry), not a grammar one — the walk reads
        // both.
        let file = parse("pane \"log\" {\n    script \"#!/bin/sh\"\n}\n").expect("parses");
        assert_eq!(
            file.panes[0].script.as_ref().map(Template::as_str),
            Some("#!/bin/sh")
        );
    }

    #[test]
    fn a_multi_line_script_body_reaches_the_declaration_dedented() {
        // The measured kdl fact: the closing line's indentation is
        // removed from every line — which is why the docs must teach
        // "align the closing quotes with the script".
        let file = parse(
            "\npane \"log\" {\n    script \"\"\"\n        #!/usr/bin/env fish\n        echo hi\n        \"\"\"\n}\n",
        )
        .expect("parses");
        assert_eq!(
            file.panes[0].script.as_ref().map(Template::as_str),
            Some("#!/usr/bin/env fish\necho hi")
        );
    }

    #[test]
    fn a_script_body_may_be_written_as_a_property() {
        let file = parse(r##"pane "log" script="#!/bin/sh\necho hi""##).expect("parses");
        assert_eq!(
            file.panes[0].script.as_ref().map(Template::as_str),
            Some("#!/bin/sh\necho hi")
        );
    }

    #[test]
    fn a_raw_script_body_keeps_its_backslashes() {
        // #"""…"""# is the form for sed/awk/regex-heavy bodies.
        let file = parse(
            "pane \"log\" {\n    script #\"\"\"\n        #!/bin/sh\n        printf '%s\\n' hi\n        \"\"\"#\n}\n",
        )
        .expect("parses");
        let body = file.panes[0]
            .script
            .as_ref()
            .map(Template::as_str)
            .expect("script");
        assert!(body.contains("%s\\n"), "{body:?}");
    }

    #[test]
    fn a_script_key_takes_exactly_one_string() {
        let err = parse("pane \"a\" {\n    script \"x\" \"y\"\n}\n").unwrap_err();
        let text = format!("{err:#}");
        assert!(
            text.contains("script") && text.contains("one string"),
            "{text}"
        );
    }

    /// I-52 at the pane's keys: a value of the wrong shape, or too many
    /// of them, is refused rather than half-read.
    #[test]
    fn a_pane_key_takes_exactly_the_values_its_shape_allows() {
        for (text, wanted) in [
            (
                "pane \"log\" {\n    command \"date\"\n    interval \"5s\" \"10s\"\n}\n",
                "one string",
            ),
            (
                "pane \"log\" {\n    command \"date\"\n    height \"7\"\n}\n",
                "one integer",
            ),
            (
                "pane \"log\" {\n    command \"date\"\n    chrome \"yes\"\n}\n",
                "#true or #false",
            ),
            (
                "pane \"log\" {\n    command \"date\"\n    shell 3\n}\n",
                "#true, #false, or one string",
            ),
            ("pane \"log\" {\n    command\n}\n", "one or more strings"),
        ] {
            let err = format!("{:#}", parse(text).unwrap_err());
            assert!(err.contains(wanted), "wanted {wanted:?} in {err}");
        }
    }

    #[test]
    fn an_id_sticks_to_unreserved_characters() {
        // RFC 3986 unreserved, one or more: every id is literally a
        // valid URI fragment, so reference syntax never needs
        // percent-encoding. Display text belongs in `title`.
        for bad in ["repo status", "a#b", "a/b", "caf\u{e9}", "a%b", ""] {
            let text = format!("pane {bad:?} {{\n    height 3\n    command \"date\"\n}}\n");
            let err = format!("{:#}", parse(&text).unwrap_err());
            assert!(err.contains("letters, digits"), "for {bad:?}: {err}");
        }
        for good in ["a", "A-1", "a.b", "under_score", "til~de", "0"] {
            let text = format!("pane {good:?} {{\n    height 3\n    command \"date\"\n}}\n");
            parse(&text).unwrap_or_else(|e| panic!("{good:?} should parse: {e:#}"));
        }
    }

    #[test]
    fn a_title_ref_parses_with_and_without_fallback_text() {
        let file = parse(
            "title ref=\"#header\"\npane \"header\" {\n    height 3\n    command \"date\"\n}\n",
        )
        .expect("parses");
        let title = file.title.expect("declared");
        assert_eq!(title.text, None);
        assert_eq!(title.reference.as_deref(), Some("header"));
        let file = parse(
            "title \"Fallback\" ref=\"#header\"\npane \"header\" {\n    height 3\n    command \"date\"\n}\n",
        )
        .expect("parses");
        let title = file.title.expect("declared");
        assert_eq!(title.text.as_ref().map(Template::as_str), Some("Fallback"));
        assert_eq!(title.reference.as_deref(), Some("header"));
    }

    #[test]
    fn a_title_ref_keeps_the_value_space_reserved() {
        // A bare string is refused so every non-fragment spelling
        // stays open for URI-references later; the empty fragment is
        // the whole document and is refused too.
        for (text, wanted) in [
            ("title ref=\"header\"\n", "write `ref=\"#header\"`"),
            ("title ref=\"#\"\n", "name a pane id"),
            ("title ref=3\n", "one string"),
            ("title bogus=\"x\"\n", "`ref`"),
            ("title\n", "a text, a ref"),
        ] {
            let err = format!("{:#}", parse(text).unwrap_err());
            assert!(err.contains(wanted), "wanted {wanted:?} in {err}");
        }
    }

    #[test]
    fn a_dashboard_title_parses_and_both_meanings_coexist() {
        // The same word at two positions carries two meanings: the
        // top-level `title` names the whole dashboard, a pane's
        // `title` labels its own border. Both survive one file.
        let file = parse(
            "title \"Deploy status\"\npane \"build\" {\n    height 3\n    command \"date\"\n    title \"Build log\"\n}\n",
        )
        .expect("parses");
        let declared = file.title.expect("declared");
        assert_eq!(
            declared.text.as_ref().map(Template::as_str),
            Some("Deploy status")
        );
        assert_eq!(declared.reference, None);
        assert_eq!(
            file.panes[0].title.as_ref().map(Template::as_str),
            Some("Build log")
        );
        // Undeclared stays absent — the row is not rendered from an
        // empty string.
        let bare = parse("pane \"a\" {\n    height 3\n    command \"date\"\n}\n").expect("parses");
        assert_eq!(bare.title, None);
    }

    #[test]
    fn the_dashboard_title_reaches_the_composition() {
        use crate::core::registry::Composition;
        let registry =
            parse("title \"Deploy status\"\npane \"a\" {\n    height 3\n    command \"date\"\n}\n")
                .expect("parses")
                .into_registry()
                .expect("validates");
        let Composition::Panes { title, .. } = registry.composition() else {
            panic!("a dashboard registry composes panes");
        };
        assert_eq!(
            *title,
            crate::core::registry::TitleSource::Static("Deploy status".to_string())
        );
        let registry = parse("pane \"a\" {\n    height 3\n    command \"date\"\n}\n")
            .expect("parses")
            .into_registry()
            .expect("validates");
        let Composition::Panes { title, .. } = registry.composition() else {
            panic!("a dashboard registry composes panes");
        };
        assert_eq!(*title, crate::core::registry::TitleSource::None);
    }

    #[test]
    fn a_title_ref_binds_the_first_declaration_and_an_unknown_ref_teaches() {
        use crate::core::registry::{Composition, SourceId, TitleSource};
        // First-win, the same rule duplicates follow everywhere.
        let registry = parse(
            "title ref=\"#x\"\npane \"x\" {\n    height 3\n    command \"date\"\n}\npane \"x\" {\n    height 3\n    command \"uptime\"\n}\n",
        )
        .expect("parses")
        .into_registry()
        .expect("validates");
        let Composition::Panes { title, .. } = registry.composition() else {
            panic!("panes")
        };
        assert_eq!(
            *title,
            TitleSource::Pane {
                source: SourceId(0),
                fallback: None
            }
        );
        // An id nothing declares is a load error that lists what exists.
        let err = format!(
            "{:#}",
            parse("title ref=\"#nope\"\npane \"a\" {\n    height 3\n    command \"date\"\n}\n")
                .expect("parses")
                .into_registry()
                .unwrap_err()
        );
        assert!(err.contains("names no pane"), "{err}");
        assert!(err.contains("declared ids are a"), "{err}");
    }

    #[test]
    fn title_takes_one_string_and_nothing_else() {
        for (text, wanted) in [
            ("title x=\"y\"\n", "`ref`"),
            ("title \"a\" \"b\"\n", "one string"),
            ("title 3\n", "one string"),
            ("title \"a\" {\n}\n", "holds no block"),
            ("(u8)title \"a\"\n", "type annotation"),
            ("title (u8)\"a\"\n", "type annotation"),
        ] {
            let err = format!("{:#}", parse(text).unwrap_err());
            assert!(err.contains(wanted), "wanted {wanted:?} in {err}");
        }
    }

    #[test]
    fn gap_takes_one_integer_and_nothing_else() {
        for (text, wanted) in [
            ("gap x=1\n", "no properties"),
            ("gap 1 2\n", "one integer"),
            ("gap \"1\"\n", "one integer"),
            ("gap -1\n", "non-negative"),
        ] {
            let err = format!("{:#}", parse(text).unwrap_err());
            assert!(err.contains(wanted), "wanted {wanted:?} in {err}");
        }
    }

    #[test]
    fn defaults_takes_no_id() {
        let err = format!(
            "{:#}",
            parse("defaults \"x\" {\n    height 3\n}\n").unwrap_err()
        );
        assert!(err.contains("no id"), "{err}");
    }

    /// The top-level pane name was read with a first-one-wins helper, so
    /// a second name or a non-string was silently dropped.
    #[test]
    fn a_top_level_pane_takes_exactly_one_string_name() {
        for (text, wanted) in [
            ("pane \"a\" \"b\" {\n    height 3\n}\n", "ONE id"),
            ("pane 3 {\n    height 3\n}\n", "is a string"),
            ("(u8)pane \"a\" {\n    height 3\n}\n", "type annotation"),
        ] {
            let err = format!("{:#}", parse(text).unwrap_err());
            assert!(err.contains(wanted), "wanted {wanted:?} in {err}");
        }
    }

    /// I-52 reaches the key node itself, not just the block holding it.
    /// A key node carries its value and nothing else — a property, a
    /// block, or an annotation hung on it is a token with nowhere to go.
    #[test]
    fn a_key_node_carries_its_value_and_nothing_else() {
        for (tail, wanted) in [
            (
                "interval \"5s\" bogus=\"x\"",
                "pane \"log\": `interval` takes one string, but \"bogus\" is set — write `interval \"5s\"`",
            ),
            (
                "interval \"5s\" { junk \"x\" }",
                "pane \"log\": `interval` takes one string and holds no block — write `interval \"5s\"`",
            ),
            // An EMPTY block is still a block the author wrote. The
            // message says the key holds none, so accepting `{}` would
            // make the parser contradict its own error.
            (
                "interval \"5s\" {}",
                "pane \"log\": `interval` takes one string and holds no block — write `interval \"5s\"`",
            ),
            (
                "(u8)interval \"5s\"",
                "pane \"log\": the (u8) type annotation on `interval` has no meaning here — remove it",
            ),
            (
                "interval (string)\"5s\"",
                "pane \"log\": the (string) type annotation on `interval` has no meaning here — remove it",
            ),
        ] {
            assert_eq!(
                container_err(&format!(
                    "pane \"log\" {{\n    height 3\n    {tail}\n    command \"date\"\n}}\n"
                )),
                wanted
            );
        }
    }

    /// D-7 has no exceptions: an annotation means nothing to this
    /// grammar on ANY node, and a container or a name is a node like the
    /// rest. The key nodes were covered first and these were not, which
    /// is the same hole in a different position.
    #[test]
    fn an_annotation_is_refused_on_a_container_or_a_name() {
        for (text, wanted) in [
            (
                "(u8)row {\n    pane \"a\" { height 3; command \"date\" }\n}\n",
                "row #1: the (u8) type annotation on `row` has no meaning here — remove it",
            ),
            (
                "row {\n    (u8)column { pane \"a\" { height 3; command \"date\" } }\n}\n",
                "row #1 > column #1: the (u8) type annotation on `column` has no meaning here — remove it",
            ),
            (
                "(u8)defaults { height 3 }\npane \"a\" { command \"date\" }\n",
                "defaults: the (u8) type annotation on `defaults` has no meaning here — remove it",
            ),
            (
                "pane (name)\"x\" {\n    height 3\n    command \"date\"\n}\n",
                "pane #1: the (name) type annotation on a pane has no meaning here — remove it",
            ),
            (
                "row {\n    pane (name)\"x\" { height 3; command \"date\" }\n}\n",
                "row #1 > pane #1: the (name) type annotation on a pane has no meaning here — remove it",
            ),
        ] {
            assert_eq!(container_err(text), wanted);
        }
    }

    /// A document setting is a key too: `gap` holds one integer, so a
    /// block or an annotation on it reaches nothing.
    #[test]
    fn a_document_setting_carries_its_value_and_nothing_else() {
        for block in ["{ junk \"x\" }", "{}"] {
            assert_eq!(
                container_err(&format!(
                    "gap 1 {block}\npane \"a\" {{ height 3; command \"date\" }}\n"
                )),
                "gap takes one integer and holds no block — write `gap 1`"
            );
        }
        assert_eq!(
            container_err("(u8)gap 1\npane \"a\" { height 3; command \"date\" }\n"),
            "the (u8) type annotation on `gap` has no meaning here — remove it"
        );
    }

    /// I-55 at the document level: last-wins is as invisible here as it
    /// is inside a pane block, and a second `defaults` silently
    /// replacing the first is the same silent discard in a new place.
    #[test]
    fn a_document_setting_is_declared_once() {
        for (text, wanted) in [
            (
                "gap 1\ngap 5\npane \"a\" { height 3; command \"date\" }\n",
                "`gap` is declared twice — a dashboard declares it once",
            ),
            (
                "row-gap 1\nrow-gap 5\npane \"a\" { height 3; command \"date\" }\n",
                "`row-gap` is declared twice — a dashboard declares it once",
            ),
            (
                "defaults { height 3 }\ndefaults { height 9 }\npane \"a\" { command \"date\" }\n",
                "`defaults` is declared twice — a dashboard declares it once",
            ),
            (
                "title \"a\"\ntitle \"b\"\npane \"a\" { height 3; command \"date\" }\n",
                "`title` is declared twice — a dashboard declares it once",
            ),
        ] {
            assert_eq!(container_err(text), wanted);
        }
    }

    /// Slashdash is the kdl crate's job, and it does it before the walk
    /// reaches us — commenting a key out must keep working.
    #[test]
    fn a_commented_out_key_is_not_a_declaration() {
        let file = parse(
            "pane \"log\" /-interval=\"15s\" {\n    command \"date\"\n    /-command \"old\"\n}\n",
        )
        .expect("parses");
        assert_eq!(file.panes[0].interval, None);
        assert_eq!(file.panes[0].command, Some(vec!["date".into()]));
    }

    /// The boundary the "holds no block" rule must not cross: a block
    /// the author COMMENTED OUT was not written, so refusing it would
    /// punish the ordinary way of temporarily removing something.
    #[test]
    fn a_commented_out_block_is_not_a_block() {
        let file = parse(
            "pane \"log\" {\n    height 3\n    interval \"5s\" /-{ junk \"x\" }\n    command \"date\"\n}\n",
        )
        .expect("a slashdashed block is not a block");
        assert_eq!(
            file.panes[0].interval.as_ref().map(Template::as_str),
            Some("5s")
        );
    }

    #[test]
    fn a_kdl_type_annotation_is_refused() {
        let err = format!(
            "{:#}",
            parse("pane \"log\" height=(i64)7 {\n    command \"date\"\n}\n").unwrap_err()
        );
        assert!(err.contains("type annotation"), "{err}");
        assert!(err.contains("(i64)"), "{err}");
    }

    /// The C equivalence proof: a scalar key means the same thing on
    /// the block's own line as inside it. Author's choice, uniformly.
    #[test]
    fn a_scalar_key_may_be_written_as_a_property_or_a_child_node() {
        let as_properties = parse(
            r#"
pane "log" interval="15s" height=7 width="2fr" chrome=#false focusable=#false {
    command "git" "log"
}
"#,
        )
        .expect("properties parse");
        let as_children = parse(
            r#"
pane "log" {
    interval "15s"
    height 7
    width "2fr"
    chrome #false
    focusable #false
    command "git" "log"
}
"#,
        )
        .expect("children parse");
        assert_eq!(as_properties, as_children);
        assert_eq!(
            as_properties.panes[0]
                .interval
                .as_ref()
                .map(Template::as_str),
            Some("15s")
        );
        assert_eq!(as_properties.panes[0].height, Some(7));
    }

    #[test]
    fn defaults_collapses_to_one_line_of_properties() {
        let one_line =
            parse("defaults interval=\"5s\" border=\"rounded\" padding=\"0 1\" height=7\n")
                .expect("parses");
        let block = parse(
            "defaults {\n    interval \"5s\"\n    border \"rounded\"\n    padding \"0 1\"\n    height 7\n}\n",
        )
        .expect("parses");
        assert_eq!(one_line, block);
        assert_eq!(
            one_line.defaults.border.as_ref().map(Template::as_str),
            Some("rounded")
        );
    }

    /// A KDL property holds exactly one value, so the two list keys
    /// have no property spelling — and the error says where they go.
    #[test]
    fn a_list_key_as_a_property_says_where_it_belongs() {
        for text in [
            "pane \"log\" command=\"git log\" {\n    height 3\n}\n",
            "pane \"log\" trigger=\"file:./x\" {\n    command \"date\"\n}\n",
        ] {
            let err = format!("{:#}", parse(text).unwrap_err());
            assert!(err.contains("holds a list"), "{err}");
            assert!(err.contains("child node"), "{err}");
            assert!(err.contains("inside the block"), "{err}");
        }
    }

    #[test]
    fn an_unknown_property_names_the_keys_that_may_be_properties() {
        let err = format!(
            "{:#}",
            parse("pane \"log\" intervl=\"15s\" {\n    command \"date\"\n}\n").unwrap_err()
        );
        assert!(err.contains("unknown property"), "{err}");
        assert!(err.contains("intervl"), "{err}");
        assert!(err.contains("interval"), "{err}");
        assert!(
            !err.contains("command"),
            "a list key has no property spelling, so it must not be offered: {err}"
        );
    }

    /// One key, one place, once — in any spelling combination. Last-wins
    /// is invisible to a reader scanning a long pane block.
    #[test]
    fn a_key_declared_twice_on_one_pane_is_refused() {
        for text in [
            // twice as a property
            "pane \"log\" interval=\"15s\" interval=\"30s\" {\n    command \"date\"\n}\n",
            // twice as a child node
            "pane \"log\" {\n    command \"date\"\n    interval \"15s\"\n    interval \"30s\"\n}\n",
            // once each
            "pane \"log\" interval=\"15s\" {\n    command \"date\"\n    interval \"30s\"\n}\n",
        ] {
            let err = format!("{:#}", parse(text).unwrap_err());
            assert!(err.contains("declared twice"), "{err}");
            assert!(err.contains("interval"), "{err}");
        }
    }

    #[test]
    fn a_property_carries_a_kdl_boolean_not_a_quoted_string() {
        let err = format!(
            "{:#}",
            parse("pane \"log\" chrome=\"false\" {\n    command \"date\"\n}\n").unwrap_err()
        );
        assert!(err.contains("#false"), "{err}");
        let ok = parse("pane \"log\" chrome=#false {\n    command \"date\"\n}\n").expect("parses");
        assert_eq!(ok.panes[0].chrome, Some(false));
    }

    /// `shell` is read before the pass that assigns it, because the
    /// command split depends on it — and the property spelling must
    /// reach that read, not just the field.
    #[test]
    fn a_property_holds_the_same_shell_the_command_split_reads() {
        let file = parse("pane \"x\" shell=#true {\n    command \"date +%H | tr -d x\"\n}\n")
            .expect("parses");
        assert_eq!(file.panes[0].shell, Some(ShellDecl::Platform));
        assert_eq!(
            file.panes[0].command,
            Some(vec!["date +%H | tr -d x".into()]),
            "one word under shell stays one word"
        );
        // The split follows runs_a_shell, never WHICH shell: a named
        // shell keeps its script one word exactly as #true does.
        let file = parse("pane \"x\" shell=\"fish\" {\n    command \"date +%H | tr -d x\"\n}\n")
            .expect("parses");
        assert_eq!(file.panes[0].shell, Some(ShellDecl::Named("fish".into())));
        assert_eq!(
            file.panes[0].command,
            Some(vec!["date +%H | tr -d x".into()]),
            "one word under a named shell stays one word"
        );
    }

    /// `#true` is the platform's shell, `#false` no shell at all, and a
    /// string names the program — in both spellings, both positions.
    #[test]
    fn a_shell_key_names_the_shell_it_runs() {
        for (text, wanted) in [
            (
                "pane \"x\" shell=#true {\n    command \"date\"\n}\n",
                ShellDecl::Platform,
            ),
            (
                "pane \"x\" {\n    shell #true\n    command \"date\"\n}\n",
                ShellDecl::Platform,
            ),
            (
                "pane \"x\" shell=#false {\n    command \"date\"\n}\n",
                ShellDecl::Direct,
            ),
            (
                "pane \"x\" shell=\"fish\" {\n    command \"date\"\n}\n",
                ShellDecl::Named("fish".into()),
            ),
            (
                "pane \"x\" {\n    shell \"fish\"\n    command \"date\"\n}\n",
                ShellDecl::Named("fish".into()),
            ),
        ] {
            let file = parse(text).expect("parses");
            assert_eq!(file.panes[0].shell, Some(wanted.clone()), "{text}");
        }
        // An EMPTY string names nothing — a shape error naming the
        // accepted values, never a spawn of `""`.
        for text in [
            "pane \"x\" shell=\"\" {\n    command \"date\"\n}\n",
            "pane \"x\" shell=\"   \" {\n    command \"date\"\n}\n",
            "pane \"x\" {\n    shell \"\"\n    command \"date\"\n}\n",
        ] {
            let err = format!("{:#}", parse(text).unwrap_err());
            assert!(err.contains("#true, #false, or one string"), "{err}");
        }
    }

    /// The teaching error names the pane it happened in, the token the
    /// user wrote, and the keys they meant — all three read off the one
    /// table. Supersedes `an_unknown_kdl_node_names_the_accepted_set`,
    /// which could not name the pane.
    #[test]
    fn an_unknown_pane_key_names_the_pane_and_the_keys() {
        let err = format!(
            "{:#}",
            parse("pane \"log\" {\n    comand \"date\"\n    height 3\n}\n").unwrap_err()
        );
        assert!(err.contains("log"), "names the pane: {err}");
        assert!(err.contains("comand"), "quotes what was written: {err}");
        assert!(err.contains("command"), "{err}");
        assert!(err.contains("interval"), "{err}");
    }

    // ─── The `variables` block ──────────────────────────────────────

    use crate::core::registry::ShellDecl;
    use crate::core::template::{Bindings, Template};
    use crate::core::variables::{
        Derivation, Expanded, Resolved, Tier, VarSource, VariableBlock, opaque_roots,
        resolve_partial, resolve_variables,
    };

    fn vars(text: &str) -> VariableBlock {
        parse(text).expect("the board parses").variables
    }

    fn vars_err(text: &str) -> String {
        format!("{:#}", parse(text).expect_err("the board is refused"))
    }

    /// The smallest legal board to hang a `variables` block on.
    const ONE_PANE: &str = "\npane \"a\" { command \"true\"\nheight 3 }\n";

    #[test]
    fn a_variables_block_declares_constants_and_commands() {
        let block = vars(&format!(
            r#"
variables {{
    plan  "/tmp/plans/0028"
    store "git rev-parse --git-common-dir" shell=#true
    head  "git rev-parse --short HEAD" shell=#true defer=#true
    fishy "status --porcelain" shell="fish"
}}
{ONE_PANE}"#
        ));
        assert_eq!(
            block.get("plan").map(|v| &v.source),
            Some(&VarSource::Constant)
        );
        assert_eq!(
            block.get("store").map(|v| v.source.clone()),
            Some(VarSource::LoadCommand(ShellDecl::Platform))
        );
        assert_eq!(
            block.get("head").map(|v| v.source.clone()),
            Some(VarSource::SpawnCommand(ShellDecl::Platform))
        );
        assert_eq!(
            block.get("fishy").map(|v| v.source.clone()),
            Some(VarSource::LoadCommand(ShellDecl::Named(Template::extract(
                "fish"
            ))))
        );
        // The text is the author's bytes, unexpanded — this walk never
        // expands (INV-2).
        assert_eq!(
            block.get("plan").map(|v| v.text.as_str()),
            Some("/tmp/plans/0028")
        );
        assert_eq!(block.declared_list(), "fishy, head, plan, store");
    }

    #[test]
    fn a_board_with_no_variables_block_has_an_empty_map() {
        let block = vars(ONE_PANE);
        assert!(block.is_empty());
        assert_eq!(block.declared_list(), "");
    }

    #[test]
    fn a_reference_resolves_regardless_of_where_it_is_written() {
        // The unordered-references regression test: `cur` is written
        // ABOVE the `sel` it
        // references, and above the `store` that `sel` needs. Under the
        // retired declaration-order rule this was an unknown-variable
        // error for a name visible three lines down.
        let block = vars(&format!(
            r#"
variables {{
    cur   "{{{{sel}}}}.cursor"
    sel   "{{{{store}}}}/pointbreak-review.sel"
    store "/tmp/common"
}}
{ONE_PANE}"#
        ));
        let order: Vec<&str> = block.in_order().map(|v| v.name.as_str()).collect();
        // Topological: every name appears after everything it references.
        assert_eq!(order, vec!["store", "sel", "cur"]);
        assert_eq!(
            block.get("cur").map(|v| v.text.refs()),
            Some(&["sel".to_string()][..])
        );
    }

    #[test]
    fn a_self_reference_is_refused_and_names_the_cycle() {
        let err = vars_err(&format!(
            "variables {{\n    a \"{{{{a}}}}/x\"\n}}\n{ONE_PANE}"
        ));
        assert!(err.contains("variable cycle: `a` → `a`"), "got {err}");
        assert!(
            err.starts_with("line 2, column "),
            "the error is placed: {err}"
        );
    }

    #[test]
    fn a_two_hop_cycle_is_refused_with_the_whole_path() {
        let err = vars_err(&format!(
            "variables {{\n    a \"{{{{b}}}}\"\n    b \"{{{{a}}}}\"\n}}\n{ONE_PANE}"
        ));
        assert!(err.contains("variable cycle: `a` → `b` → `a`"), "got {err}");
    }

    #[test]
    fn a_three_hop_cycle_renders_every_hop() {
        // Beyond the degenerate case: the path RENDERER is what is under
        // test, not the detector.
        let err = vars_err(&format!(
            "variables {{\n    a \"{{{{b}}}}\"\n    b \"{{{{c}}}}\"\n    c \"{{{{a}}}}\"\n}}\n{ONE_PANE}"
        ));
        assert!(
            err.contains("variable cycle: `a` → `b` → `c` → `a`"),
            "got {err}"
        );
    }

    #[test]
    fn a_reference_to_an_undeclared_name_inside_the_block_is_refused() {
        let err = vars_err(&format!(
            "variables {{\n    a \"{{{{nope}}}}\"\n    b \"x\"\n}}\n{ONE_PANE}"
        ));
        assert!(err.contains("unknown variable `nope`"), "got {err}");
        assert!(err.contains("declared variables are a, b"), "got {err}");
    }

    #[test]
    fn a_constant_that_references_a_command_is_promoted_within_the_load_tier() {
        // INV-3's own canonical example, and the repeated-derived-path
        // case variables exist for.
        // A literal reading of the tier ladder over DECLARED forms would
        // refuse this; over EFFECTIVE tiers it is legal, and `sel` simply
        // IS once-at-load, which is also the truth.
        let block = vars(&format!(
            r#"
variables {{
    sel   "{{{{store}}}}/pointbreak-review.sel"
    store "git rev-parse --git-common-dir" shell=#true
}}
{ONE_PANE}"#
        ));
        assert_eq!(block.tier("sel"), Some(Tier::Load));
        assert_eq!(block.tier("store"), Some(Tier::Load));
    }

    #[test]
    fn every_load_to_load_pairing_is_accepted() {
        // INV-9's accepting rows, one board each: constant→constant,
        // once-at-load→once-at-load, deferred→deferred, deferred→
        // once-at-load, deferred→constant.
        for (body, name, tier) in [
            ("a \"x\"\n    b \"{{a}}\"", "b", Tier::Load),
            (
                "a \"echo x\" shell=#true\n    b \"echo {{a}}\" shell=#true",
                "b",
                Tier::Load,
            ),
            (
                "a \"echo x\" shell=#true defer=#true\n    b \"echo {{a}}\" shell=#true defer=#true",
                "b",
                Tier::Spawn,
            ),
            (
                "a \"echo x\" shell=#true\n    b \"echo {{a}}\" shell=#true defer=#true",
                "b",
                Tier::Spawn,
            ),
            (
                "a \"x\"\n    b \"echo {{a}}\" shell=#true defer=#true",
                "b",
                Tier::Spawn,
            ),
        ] {
            let block = vars(&format!("variables {{\n    {body}\n}}\n{ONE_PANE}"));
            assert_eq!(block.tier(name), Some(tier), "{body}");
        }
    }

    #[test]
    fn a_load_time_variable_referencing_a_deferred_one_is_refused_in_either_order() {
        // A TIER violation, not a position rule: it holds regardless of
        // where the two are written, which is the property INV-3's
        // unordered references made testable.
        for body in [
            "sel  \"{{head}}/x\"\n    head \"git rev-parse --short HEAD\" shell=#true defer=#true",
            "head \"git rev-parse --short HEAD\" shell=#true defer=#true\n    sel  \"{{head}}/x\"",
        ] {
            let err = vars_err(&format!("variables {{\n    {body}\n}}\n{ONE_PANE}"));
            assert!(
                err.contains("`sel` is not deferred but references `head`, which is"),
                "names BOTH variables: {err}"
            );
            assert!(
                err.contains("add `defer=#true` to `sel`") && err.contains("drop it from `head`"),
                "teaches both fixes: {err}"
            );
        }
    }

    #[test]
    fn an_effective_tier_never_outruns_its_declared_form() {
        // The pin INV-7's ONE-LEVEL site check rests on. Because
        // clause 2 refuses load-references-deferred, a variable's
        // effective tier always EQUALS its declared tier once the block
        // is accepted — so "is any DIRECTLY referenced name deferred?" is
        // a complete answer at a site, and no transitive walk is needed.
        // If someone ever relaxes clause 2 to auto-promotion, this is the
        // test that fails first.
        let block = vars(&format!(
            r#"
variables {{
    k "x"
    l "{{{{k}}}}"
    m "echo {{{{l}}}}" shell=#true
    n "echo {{{{m}}}}" shell=#true defer=#true
    o "echo {{{{n}}}}" shell=#true defer=#true
}}
{ONE_PANE}"#
        ));
        for v in block.in_order() {
            assert_eq!(v.tier, v.source.declared_tier(), "{}", v.name);
        }
    }

    #[test]
    fn a_templated_shell_name_is_a_dependency_like_any_other() {
        // A dialect name is a string an author writes, so it carries
        // references — and a variable cannot be derived until the shell
        // that runs it is known. It is therefore a graph EDGE, ordered
        // like any other, and it participates in tier analysis.
        let block = vars(&format!(
            r#"
variables {{
    dialect "fish"
    out     "status --porcelain" shell="{{{{dialect}}}}"
}}
{ONE_PANE}"#
        ));
        let order: Vec<&str> = block.in_order().map(|v| v.name.as_str()).collect();
        assert_eq!(order, vec!["dialect", "out"]);
        assert_eq!(
            block.get("out").unwrap().refs().collect::<Vec<_>>(),
            vec!["dialect"]
        );

        // An unknown name in a shell is the ordinary INV-6 refusal…
        let err = vars_err(&format!(
            "variables {{\n    out \"x\" shell=\"{{{{nope}}}}\"\n}}\n{ONE_PANE}"
        ));
        assert!(err.contains("unknown variable `nope`"), "got {err}");

        // …a cycle through a shell is still a cycle…
        let err = vars_err(&format!(
            "variables {{\n    a \"x\" shell=\"{{{{b}}}}\"\n    b \"{{{{a}}}}\"\n}}\n{ONE_PANE}"
        ));
        assert!(err.contains("variable cycle: `a` → `b` → `a`"), "got {err}");

        // …and a shell naming a DEFERRED variable is INV-9 clause 2. This
        // is the route that slips past entirely if the graph reads only
        // `text.refs`.
        let err = vars_err(&format!(
            "variables {{\n    d \"echo fish\" shell=#true defer=#true\n    a \"x\" shell=\"{{{{d}}}}\"\n}}\n{ONE_PANE}"
        ));
        assert!(
            err.contains("`a` is not deferred but references `d`, which is"),
            "got {err}"
        );
    }

    #[test]
    fn a_shell_declaration_keeps_its_switch_forms_and_refuses_a_nameless_one() {
        let block = vars(&format!(
            "variables {{\n    p \"echo x\" shell=#true\n    f \"echo x\" shell=\"fish\"\n}}\n{ONE_PANE}"
        ));
        assert_eq!(
            block.get("p").unwrap().source.shell(),
            Some(&ShellDecl::Platform)
        );
        assert_eq!(
            block.get("f").unwrap().source.shell(),
            Some(&ShellDecl::Named(Template::extract("fish")))
        );
        assert!(
            block
                .get("p")
                .unwrap()
                .source
                .shell()
                .unwrap()
                .refs()
                .is_empty()
        );
        // An empty name names nothing — never a spawn of `""`.
        let err = vars_err(&format!(
            "variables {{\n    a \"x\" shell=\"\"\n}}\n{ONE_PANE}"
        ));
        assert!(
            err.contains("`shell` takes #true or one string"),
            "got {err}"
        );
    }

    #[test]
    fn a_duplicate_variable_name_is_refused() {
        let err = vars_err(&format!(
            "variables {{\n    plan \"a\"\n    plan \"b\"\n}}\n{ONE_PANE}"
        ));
        assert!(err.contains("`plan` is declared twice"), "got {err}");
    }

    #[test]
    fn a_variables_block_refuses_the_shapes_that_mean_nothing() {
        for (body, needle) in [
            // A name that cannot be REFERENCED cannot be declared: same
            // identifier grammar as `{{name}}` (INV-1).
            (
                "variables {\n    \"2fast\" \"x\"\n}",
                "a variable's name starts with a letter or `_`",
            ),
            // One value, a string.
            ("variables {\n    a\n}", "`a` takes one string"),
            ("variables {\n    a \"x\" \"y\"\n}", "`a` takes one string"),
            ("variables {\n    a 7\n}", "`a` takes one string"),
            // A variable holds a value, never a block.
            ("variables {\n    a \"x\" { b \"y\" }\n}", "holds no block"),
            // The block itself.
            (
                "variables \"x\" {\n    a \"y\"\n}",
                "`variables` takes no id",
            ),
            ("variables {\n}", "the `variables` block is empty"),
            ("variables\n", "the `variables` block is empty"),
            // Properties: the accepted set, then each dead knob.
            (
                "variables {\n    a \"x\" frobnicate=#true\n}",
                "a variable's properties are shell, defer",
            ),
            (
                "variables {\n    a \"x\" defer=#true\n}",
                "`defer` re-derives a value, and `a` derives nothing",
            ),
            (
                "variables {\n    a \"echo x\" shell=#false\n}",
                "a variable with no shell is a constant",
            ),
        ] {
            let err = vars_err(&format!("{body}\n{ONE_PANE}"));
            assert!(err.contains(needle), "{body:?} → {err}");
        }
    }

    #[test]
    fn a_default_property_names_the_override_that_replaced_it() {
        // INV-5, retired: `default` is an ordinary unknown property now,
        // and it carries the one-line hint that explains the deletion.
        let err = vars_err(&format!(
            "variables {{\n    limit \"50\" default=\"50\"\n}}\n{ONE_PANE}"
        ));
        assert!(
            err.contains("a variable's properties are shell, defer"),
            "got {err}"
        );
        assert!(
            err.contains("a constant is its own default") && err.contains("-v limit=200"),
            "the hint names the -v alternative: {err}"
        );
    }

    #[test]
    fn the_variables_block_is_declared_once() {
        let err = vars_err(&format!(
            "variables {{\n    a \"x\"\n}}\nvariables {{\n    b \"y\"\n}}\n{ONE_PANE}"
        ));
        assert!(err.contains("`variables` is declared twice"), "got {err}");
    }

    #[test]
    fn resolving_walks_dependencies_first_and_never_runs_an_overridden_command() {
        // The evaluator, against a COUNTING stub runner — Phase 1 has no
        // shell runner, and this is the shape the real one slots into.
        let block = vars(&format!(
            r#"
variables {{
    sel   "{{{{store}}}}/x"
    store "git rev-parse --git-common-dir" shell=#true
    other "echo hi" shell=#true
}}
{ONE_PANE}"#
        ));
        let mut asked: Vec<String> = Vec::new();
        let out = resolve_variables(&block, &Bindings::new(), &mut |d: Derivation<'_>| {
            asked.push(d.name.to_string());
            Ok(format!("<{}>", d.command))
        })
        .expect("resolves");
        assert_eq!(asked, vec!["store", "other"]);
        assert_eq!(
            out.get("store").map(String::as_str),
            Some("<git rev-parse --git-common-dir>")
        );
        // Dependencies first: `sel` saw `store`'s DERIVED value.
        assert_eq!(
            out.get("sel").map(String::as_str),
            Some("<git rev-parse --git-common-dir>/x")
        );

        // An override wins for the exact name and the command is
        // suppressed by never being reached (INV-4.4).
        let mut asked: Vec<String> = Vec::new();
        let overrides = Bindings::from([("store".to_string(), "/common".to_string())]);
        let out = resolve_variables(&block, &overrides, &mut |d: Derivation<'_>| {
            asked.push(d.name.to_string());
            Ok("unreachable".to_string())
        })
        .expect("resolves");
        assert_eq!(asked, vec!["other"], "`store`'s command never ran");
        assert_eq!(out.get("sel").map(String::as_str), Some("/common/x"));
    }

    #[test]
    fn partial_resolution_knows_constants_and_chains_and_stays_opaque_past_a_command() {
        // Task 4.1's provider: `check` runs nothing, so a command is
        // Opaque — but a constant, a chain of constants, and anything a
        // `-v` supplies are all KNOWN, and a board that is
        // deterministically wrong must still be catchable.
        let block = vars(&format!(
            r#"
variables {{
    plan  "/tmp/plans/0028"
    sub   "{{{{plan}}}}/tasks"
    store "git rev-parse --git-common-dir" shell=#true
    sel   "{{{{store}}}}/x"
    head  "git rev-parse HEAD" shell=#true defer=#true
}}
{ONE_PANE}"#
        ));
        let partial = resolve_partial(&block, &Bindings::new());
        assert_eq!(
            partial.get("plan"),
            Some(&Resolved::Known("/tmp/plans/0028".into()))
        );
        // A chain of constants is Known all the way down.
        assert_eq!(
            partial.get("sub"),
            Some(&Resolved::Known("/tmp/plans/0028/tasks".into()))
        );
        assert_eq!(partial.get("store"), Some(&Resolved::Opaque));
        // Opacity is TRANSITIVE, in the same topological pass.
        assert_eq!(partial.get("sel"), Some(&Resolved::Opaque));
        // A deferred name is PRESENT and Opaque — known-to-exist but
        // unknowable, which is a different answer from missing.
        assert_eq!(partial.get("head"), Some(&Resolved::Opaque));

        // `-v` suppresses the command, so it makes a command variable
        // knowable — and everything downstream with it.
        let partial = resolve_partial(
            &block,
            &Bindings::from([("store".into(), "/common".into())]),
        );
        assert_eq!(
            partial.get("store"),
            Some(&Resolved::Known("/common".into()))
        );
        assert_eq!(
            partial.get("sel"),
            Some(&Resolved::Known("/common/x".into()))
        );
    }

    #[test]
    fn a_partial_expansion_is_either_exact_bytes_or_names_what_blocked_it() {
        let block = vars(&format!(
            "variables {{\n    iv \"bad\"\n    store \"git rev-parse\" shell=#true\n}}\n{ONE_PANE}"
        ));
        let partial = resolve_partial(&block, &Bindings::new());
        // The case that must NOT be skipped: a deterministically wrong
        // constant. `check` hands these bytes to `parse_interval` and
        // reports the real error.
        assert_eq!(
            Template::extract("{{iv}}").expand_partial(&partial),
            Expanded::Known("bad".to_string())
        );
        // The case that must be skipped, naming exactly what blocked it.
        assert_eq!(
            Template::extract("file:{{store}}/events").expand_partial(&partial),
            Expanded::Skipped(vec!["store".to_string()])
        );
        // A template-free string, and every RAW string, are always exact.
        assert_eq!(
            Template::extract("file:./stamp").expand_partial(&partial),
            Expanded::Known("file:./stamp".to_string())
        );
        assert_eq!(
            Template::literal("file:{{store}}").expand_partial(&partial),
            Expanded::Known("file:{{store}}".to_string())
        );
    }

    #[test]
    fn opacity_traces_back_to_the_command_that_caused_it() {
        // `Skipped` names what a site DIRECTLY referenced, which points at
        // the wrong line when that name is a tainted constant. A report
        // must name the command the reader can act on.
        let block = vars(&format!(
            "variables {{\n    store \"git rev-parse\" shell=#true\n    sel \"{{{{store}}}}/x\"\n    p \"lit\"\n}}\n{ONE_PANE}"
        ));
        let partial = resolve_partial(&block, &Bindings::new());
        assert_eq!(
            Template::extract("{{sel}}").expand_partial(&partial),
            Expanded::Skipped(vec!["sel".to_string()])
        );
        assert_eq!(
            opaque_roots(&block, &partial, "sel"),
            vec!["store".to_string()]
        );
        assert_eq!(
            opaque_roots(&block, &partial, "store"),
            vec!["store".to_string()]
        );
        assert!(opaque_roots(&block, &partial, "p").is_empty());
    }

    #[test]
    fn every_known_value_on_a_mixed_board_matches_the_real_one() {
        // The property `check` actually rests on, and the one that can
        // drift: a board where opacity EXISTS in the graph, so `Known`
        // propagation has to route around it. If a `Known` value here can
        // differ from the real one, `check` does not merely miss an error
        // — it reports a WRONG one, refusing a board that runs fine.
        let block = vars(&format!(
            r#"
variables {{
    store "git rev-parse --git-common-dir" shell=#true
    plan  "/a/b"
    sel   "{{{{plan}}}}/x"
    tainted "{{{{store}}}}/y"
    head  "git rev-parse HEAD" shell=#true defer=#true
}}
{ONE_PANE}"#
        ));
        let full = resolve_variables(&block, &Bindings::new(), &mut |d: Derivation<'_>| {
            Ok(format!("<{}>", d.name))
        })
        .expect("resolves");
        let partial = resolve_partial(&block, &Bindings::new());
        // Opacity exists, so this is genuinely the mixed case.
        assert_eq!(partial.get("store"), Some(&Resolved::Opaque));
        assert_eq!(partial.get("tainted"), Some(&Resolved::Opaque));
        // …and every name partial calls Known is byte-identical to real.
        assert_eq!(partial.get("sel"), Some(&Resolved::Known("/a/b/x".into())));
        for (name, resolved) in &partial {
            if let Resolved::Known(value) = resolved {
                assert_eq!(full.get(name), Some(value), "{name}");
            }
        }
    }

    #[test]
    fn partial_and_full_resolution_agree_when_nothing_needs_running() {
        // The two modes are one walk. On a board with no command
        // variables they must produce the same bytes for every name, or
        // `check` and `rat dashboard` disagree about the same file.
        let block = vars(&format!(
            "variables {{\n    a \"x\"\n    b \"{{{{a}}}}/y\"\n}}\n{ONE_PANE}"
        ));
        let full =
            resolve_variables(&block, &Bindings::new(), &mut |_| unreachable!()).expect("resolves");
        let partial = resolve_partial(&block, &Bindings::new());
        for (name, value) in &full {
            assert_eq!(
                partial.get(name),
                Some(&Resolved::Known(value.clone())),
                "{name}"
            );
        }
    }

    #[test]
    fn a_deferred_variable_has_no_load_time_value() {
        let block = vars(&format!(
            "variables {{\n    head \"git rev-parse HEAD\" shell=#true defer=#true\n    p \"x\"\n}}\n{ONE_PANE}"
        ));
        let out = resolve_variables(&block, &Bindings::new(), &mut |_| {
            panic!("a deferred variable is never derived at load")
        })
        .expect("resolves");
        assert_eq!(out.keys().collect::<Vec<_>>(), vec!["p"]);
    }

    // ─── Name validation at every string site ───────────────────────

    #[test]
    fn an_unknown_variable_refuses_the_board_and_points_at_the_reference() {
        let err = vars_err(
            "variables {\n    plan \"/tmp/x\"\n}\n\npane \"a\" {\n    command \"true\"\n    height 3\n    title \"plan {{plna}}\"\n}\n",
        );
        assert!(err.starts_with("line 8, column "), "placed: {err}");
        assert!(
            err.contains("unknown variable `plna`"),
            "names the variable: {err}"
        );
        assert!(
            err.contains("declared variables are plan"),
            "lists the declared set: {err}"
        );
        // The house's rustc-style echo, the same contract
        // `a_kdl_syntax_error_carries_its_line_and_column` pins.
        assert!(
            err.contains("title \"plan {{plna}}\""),
            "echoes the line: {err}"
        );
    }

    #[test]
    fn a_board_with_no_variables_block_says_so_rather_than_listing_nothing() {
        let err = vars_err(
            "pane \"a\" {\n    command \"true\"\n    height 3\n    title \"{{plan}}\"\n}\n",
        );
        assert!(err.contains("unknown variable `plan`"), "got {err}");
        assert!(
            err.contains("this board declares no variables"),
            "got {err}"
        );
    }

    #[test]
    fn the_column_points_inside_the_value_at_the_reference_itself() {
        // Not at the key, not at the line: at the `{{`. The property
        // spelling and the child-node spelling both.
        let err =
            vars_err("pane \"a\" title=\"ok {{nope}}\" {\n    command \"true\"\n    height 3\n}\n");
        assert!(err.starts_with("line 1, column 20: "), "got {err}");
        // `    border "x{{nope}}"` — four spaces, `border` (5-10), a
        // space, the opening quote at 12, `x` at 13, so the `{{` opens at
        // column 14. `line_column` counts columns from 1.
        let err = vars_err(
            "pane \"a\" {\n    command \"true\"\n    height 3\n    border \"x{{nope}}\"\n}\n",
        );
        assert!(err.starts_with("line 4, column 14: "), "got {err}");
    }

    #[test]
    fn every_value_shape_route_records_and_validates() {
        // The three routes the schema's own design forces: a property
        // (`prop_text`), a child node (`one_text`), and a list
        // (`many_text` — the ONLY spelling `command` and `trigger` have).
        let file = parse(
            "variables {\n    p \"0028\"\n}\n\npane \"a\" title=\"t {{p}}\" {\n    command \"git\" \"log\" \"{{p}}\"\n    trigger \"file:{{p}}/stamp\"\n    interval \"5s\"\n    height 3\n}\n",
        )
        .expect("parses");
        let pane = &file.panes[0];
        assert_eq!(
            pane.title.as_ref().map(|t| t.refs()),
            Some(&["p".to_string()][..])
        );
        assert_eq!(
            pane.command.as_ref().unwrap()[2].refs(),
            vec!["p".to_string()]
        );
        assert_eq!(
            pane.trigger.as_ref().unwrap()[0].refs(),
            vec!["p".to_string()]
        );
        // And the walk NEVER expands (INV-2) — the bytes are the author's.
        assert_eq!(pane.title.as_ref().unwrap().as_str(), "t {{p}}");
        // A template-free value records nothing: the common case, and
        // the path spawn-time expansion short-circuits.
        assert!(pane.interval.as_ref().unwrap().refs().is_empty());

        for bad in [
            "pane \"a\" title=\"{{q}}\" { command \"true\"\nheight 3 }",
            "pane \"a\" { command \"true\"\nheight 3\ntitle \"{{q}}\" }",
            "pane \"a\" { command \"git\" \"{{q}}\"\nheight 3 }",
            "pane \"a\" { command \"true\"\nheight 3\ntrigger \"file:{{q}}\" }",
        ] {
            let err = vars_err(&format!("variables {{\n    p \"x\"\n}}\n{bad}\n"));
            assert!(err.contains("unknown variable `q`"), "{bad} → {err}");
        }
    }

    #[test]
    fn a_declaration_position_route_validates_inside_defaults_too() {
        // INV-2 claims a `{{name}}` in `defaults` and one on a pane are
        // identical, because expansion happens after inheritance either
        // way. That identity is the claim under test — here on the
        // validation half.
        let err = vars_err(
            "variables {\n    p \"x\"\n}\n\ndefaults {\n    height 3\n    border \"{{q}}\"\n}\n\npane \"a\" { command \"true\" }\n",
        );
        assert!(err.contains("unknown variable `q`"), "got {err}");
        let file = parse(
            "variables {\n    p \"x\"\n}\n\ndefaults {\n    height 3\n    border \"{{p}}\"\n}\n\npane \"a\" { command \"true\" }\n",
        )
        .expect("parses");
        assert_eq!(
            file.defaults.border.as_ref().map(|t| t.refs()),
            Some(&["p".to_string()][..])
        );
    }

    #[test]
    fn a_variables_block_written_below_its_first_reference_still_answers() {
        // Position carries no meaning anywhere in this format (INV-3).
        // The block is built before ANY string site is read, so a board
        // that declares its variables at the bottom behaves identically.
        parse("pane \"a\" {\n    command \"true\"\n    height 3\n    title \"{{p}}\"\n}\n\nvariables {\n    p \"x\"\n}\n")
            .expect("the block is found wherever it is written");
    }

    #[test]
    fn a_shell_dialect_name_is_a_string_site_like_any_other() {
        let err = vars_err(
            "variables {\n    p \"fish\"\n}\n\npane \"a\" shell=\"{{q}}\" { command \"true\"\nheight 3 }\n",
        );
        assert!(err.contains("unknown variable `q`"), "got {err}");
        // A declared one is accepted, and the SPLIT is unaffected: any
        // non-empty string is `ShellDecl::Named`, and `runs_a_shell()` is
        // true for it whatever the name expands to later.
        let file = parse(
            "variables {\n    p \"fish\"\n}\n\npane \"a\" shell=\"{{p}}\" { command \"git log -3\"\nheight 3 }\n",
        )
        .expect("parses");
        assert_eq!(
            file.panes[0].command.as_ref().unwrap().len(),
            1,
            "one word under a shell"
        );
    }

    #[test]
    fn the_dashboards_own_title_is_a_string_site_and_its_ref_is_not() {
        let err = vars_err(
            "variables {\n    p \"x\"\n}\ntitle \"board {{q}}\"\npane \"a\" { command \"true\"\nheight 3 }\n",
        );
        assert!(err.contains("unknown variable `q`"), "got {err}");
        // INV-3: a pane id is IDENTITY — the `RAT_PANE` value, the anchor
        // a `ref` binds to, a URI fragment. A computed one would make
        // `ref` unresolvable by reading the file, so a reference in a
        // `ref` is refused rather than expanded.
        let err = vars_err(
            "variables {\n    p \"a\"\n}\ntitle ref=\"#{{p}}\"\npane \"a\" { command \"true\"\nheight 3 }\n",
        );
        assert!(
            err.contains("a pane's id is not substitutable"),
            "got {err}"
        );
    }

    #[test]
    fn a_pane_id_keeps_the_refusal_it_already_had() {
        // No new rule needed: the RFC 3986 charset check in `one_id`
        // already refuses `{` and `}`, and its message already sends
        // display text to `title`, which IS substitutable.
        let err = vars_err("pane \"{{p}}\" { command \"true\"\nheight 3 }\n");
        assert!(
            err.contains("a pane's id sticks to letters, digits"),
            "got {err}"
        );
    }

    #[test]
    fn a_brace_run_that_is_no_reference_never_reaches_validation() {
        // The awk body that made `$name` unusable. It parses, records no
        // references, and keeps its bytes.
        let file = parse(
            "pane \"a\" {\n    command \"awk '{{print $1}}' f\"\n    shell #true\n    height 3\n}\n",
        )
        .expect("an awk body is not a template");
        assert!(file.panes[0].command.as_ref().unwrap()[0].refs().is_empty());
    }

    // ─── Raw strings never interpolate ──────────────────────────────

    #[test]
    fn a_raw_string_records_no_references_at_any_of_the_three_shapes() {
        // Routes 1-3: the property spelling, the child-node spelling, and
        // a list value. `p` IS declared, so a validation error cannot be
        // what keeps the braces — only raw-ness can.
        let file = parse(
            "variables {\n    p \"0028\"\n}\n\npane \"a\" title=#\"t {{p}}\"# {\n    border ##\"b {{p}}\"##\n    command \"git\" #\"log {{p}}\"#\n    height 3\n}\n",
        )
        .expect("parses");
        let pane = &file.panes[0];
        for t in [
            pane.title.as_ref().unwrap(),
            pane.border.as_ref().unwrap(),
            &pane.command.as_ref().unwrap()[1],
        ] {
            assert!(t.refs().is_empty(), "{:?} references nothing", t.as_str());
            assert!(!t.interpolates());
            // The braces survive: expansion is a no-op, whatever the map.
            let map = Bindings::from([("p".to_string(), "0028".to_string())]);
            assert_eq!(t.expand(&map).unwrap(), t.as_str());
        }
        assert_eq!(pane.title.as_ref().unwrap().as_str(), "t {{p}}");
    }

    #[test]
    fn a_raw_multiline_script_body_is_literal_end_to_end() {
        // Route 4, and the case INV-1 calls out by name: boards reach for
        // raw strings precisely for regex and awk bodies.
        let file = parse(
            "variables {\n    p \"0028\"\n}\n\npane \"a\" {\n    shell #true\n    height 3\n    script #\"\"\"\nawk '{{print $1}}' {{p}}\n\"\"\"#\n}\n",
        )
        .expect("parses");
        let body = file.panes[0].script.as_ref().expect("a script body");
        assert!(body.refs().is_empty());
        assert!(
            body.as_str().contains("{{p}}"),
            "the braces survive: {:?}",
            body.as_str()
        );
    }

    #[test]
    fn a_raw_string_is_legal_at_a_load_typed_site_even_holding_a_deferred_name() {
        // Route 5, and the INV-7 interaction: a raw string can NEVER be a
        // template, so it is trivially legal at every site including the
        // load-typed ones. This is the route that proves raw-ness is
        // decided BEFORE the site rule rather than after — the recorded
        // reference set is already empty, so the site check has nothing
        // to look at. The site-rule task re-asserts this once the rule
        // exists.
        let file = parse(
            "variables {\n    head \"git rev-parse HEAD\" shell=#true defer=#true\n}\n\npane \"a\" {\n    command \"true\"\n    height 3\n    trigger #\"file:{{head}}/stamp\"#\n    interval #\"{{head}}\"#\n}\n",
        )
        .expect("a raw string is not a template");
        assert!(file.panes[0].trigger.as_ref().unwrap()[0].refs().is_empty());
        assert!(file.panes[0].interval.as_ref().unwrap().refs().is_empty());
    }

    #[test]
    fn a_normal_multiline_string_still_interpolates() {
        // The pair that proves the rule is FLAVOR, not line count: the
        // `"""…"""` form is a normal string and behaves like one.
        let file = parse(
            "variables {\n    p \"0028\"\n}\n\npane \"a\" {\n    shell #true\n    height 3\n    script \"\"\"\necho {{p}}\n\"\"\"\n}\n",
        )
        .expect("parses");
        assert_eq!(
            file.panes[0].script.as_ref().unwrap().refs(),
            vec!["p".to_string()]
        );
    }

    #[test]
    fn a_raw_string_is_never_validated_and_so_never_refuses() {
        // The other half of raw-ness: an undeclared name inside a raw
        // string is not a name at all, so INV-6 has nothing to complain
        // about. The same bytes in a normal string refuse.
        parse("pane \"a\" {\n    command \"true\"\n    height 3\n    title #\"{{nope}}\"#\n}\n")
            .expect("a raw string references nothing");
        let err = vars_err(
            "pane \"a\" {\n    command \"true\"\n    height 3\n    title \"{{nope}}\"\n}\n",
        );
        assert!(err.contains("unknown variable `nope`"), "got {err}");
    }

    #[test]
    fn a_raw_variable_value_is_literal_too() {
        let block = vars(&format!(
            "variables {{\n    a \"x\"\n    lit #\"{{{{a}}}}\"#\n}}\n{ONE_PANE}"
        ));
        assert!(block.get("lit").unwrap().text.refs().is_empty());
        // And therefore it is not a graph edge: `lit` depends on nothing.
        assert_eq!(block.tier("lit"), Some(Tier::Load));
    }

    #[test]
    fn a_raw_values_words_stay_literal_across_the_command_split() {
        // INV-7's argv boundary rule meets INV-1: the split happens at
        // load on template text, and re-recording each word must not
        // invent a reference the whole value never had.
        let file = parse(
            "variables {\n    p \"x\"\n}\n\npane \"a\" {\n    command #\"git log {{p}}\"#\n    height 3\n}\n",
        )
        .expect("parses");
        let argv = file.panes[0].command.as_ref().unwrap();
        assert_eq!(argv.len(), 3);
        assert!(argv.iter().all(|w| w.refs().is_empty()));
        assert_eq!(argv[2].as_str(), "{{p}}");
    }

    #[test]
    fn a_raw_shell_dialect_name_is_literal_at_both_callers() {
        // `shell_decl` is the THIRD site that builds a `Template` from an
        // entry, and it has two callers — a pane and a variable. Both must
        // pick by flavor, so both are pinned.
        //
        // The stake here is higher than at the other two sites: a dialect
        // name's references are dependency-graph EDGES (`Variable::refs`)
        // and tier-analysis participants. A raw name that wrongly
        // recorded a reference could refuse a board for a cycle or an
        // INV-9 tier violation that INV-1 says cannot exist.

        // Caller 1 — a pane.
        let file = parse(
            "variables {\n    sh \"fish\"\n}\n\npane \"a\" shell=#\"{{sh}}\"# {\n    command \"git log\"\n    height 3\n}\n",
        )
        .expect("a raw dialect name references nothing");
        match file.panes[0].shell.as_ref().expect("a shell") {
            ShellDecl::Named(t) => {
                assert!(t.refs().is_empty());
                assert!(!t.interpolates());
                assert_eq!(t.as_str(), "{{sh}}");
            }
            other => panic!("expected a named dialect, got {other:?}"),
        }

        // Caller 2 — a variable, where the graph consequence lives.
        let block = vars(&format!(
            "variables {{\n    sh \"fish\"\n    out \"status\" shell=#\"{{{{sh}}}}\"#\n}}\n{ONE_PANE}"
        ));
        let out = block.get("out").expect("declared");
        assert!(out.source.shell().expect("a shell").refs().is_empty());
        // No edge, so no ordering constraint and no tier participation.
        assert_eq!(out.refs().collect::<Vec<_>>(), Vec::<&str>::new());

        // And the case that proves raw-ness is doing the work: the SAME
        // board with a normal string refuses, because `nope` is undeclared.
        let err = vars_err(&format!(
            "variables {{\n    out \"status\" shell=\"{{{{nope}}}}\"\n}}\n{ONE_PANE}"
        ));
        assert!(err.contains("unknown variable `nope`"), "got {err}");
        // …while the raw spelling of the same bytes loads.
        vars(&format!(
            "variables {{\n    out \"status\" shell=#\"{{{{nope}}}}\"#\n}}\n{ONE_PANE}"
        ));
    }

    #[test]
    fn the_flavor_check_reads_the_repr_and_falls_back_to_interpolating() {
        // The prefix rule, at every hash depth, and the `None` fallback
        // stated as a unit test rather than a comment — ratto only ever
        // parses from source, so the fallback is unreachable in practice
        // and a test is the only place it is ever exercised.
        // ORDINARY Rust strings with escaped quotes, deliberately: a Rust
        // raw literal terminates at the first quote-hash, which is exactly
        // the closer of the KDL raw string being tested — one fixture
        // would truncate its own KDL and the deeper-hash ones would not
        // compile. Escapes are ugly here and correct here.
        for (source, raw) in [
            ("a \"x\"", false),
            ("a #\"x\"#", true),
            ("a ##\"x\"##", true),
            ("a \"\"\"\nx\n\"\"\"", false),
            ("a #\"\"\"\nx\n\"\"\"#", true),
            ("a x", false),     // a bare KDL v2 string
            ("a #true", false), // a keyword, never a string
        ] {
            let doc: kdl::KdlDocument = source.parse().expect(source);
            let entry = doc.nodes()[0].entries().first().expect(source);
            assert_eq!(is_raw(entry), raw, "{source:?}");
        }
        let mut built = kdl::KdlEntry::new("x");
        built.clear_format();
        assert!(!is_raw(&built), "no format means NORMAL — never raw");
    }
}
