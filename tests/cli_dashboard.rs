mod common;

use assert_cmd::Command;

fn rat() -> Command {
    let mut cmd = common::rat();
    cmd.env_remove("NO_COLOR");
    cmd
}

/// Path to the rat binary, used as a portable child process: shell
/// utilities like sh, echo, and printf do not exist everywhere.
fn rat_bin() -> String {
    assert_cmd::cargo::cargo_bin("rat").display().to_string()
}

/// Write a fixture and hand back its path. Commands are interpolated so
/// every pane runs the rat binary under test. The backslash escape
/// keeps a Windows binary path a valid KDL quoted string.
fn fixture(dir: &std::path::Path, name: &str, body: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write fixture");
    path.display().to_string()
}

#[test]
fn a_dashboard_renders_its_panes_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            r#"
defaults {{
    height 3
    chrome #false
}}

pane "left" {{
    command "{bin}" "style" "hello"
}}

pane "right" {{
    command "{bin}" "style" "world"
}}
"#,
            bin = rat_bin().replace('\\', "\\\\")
        ),
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success()
        .stdout(predicates::str::contains("hello"))
        .stdout(predicates::str::contains("world"));
}

#[test]
fn a_dashboard_expands_child_tabs_before_boxing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("tabbed");
    std::fs::write(&output, "completed\tsuccess\ttest").expect("write tabbed output");
    let file = fixture(
        dir.path(),
        "tabbed.kdl",
        &format!(
            r#"
defaults height=1 chrome=#false border="none" padding="0" width="32"

pane "ci" {{
    command "{bin}" "__cat" "{output}"
}}
"#,
            bin = rat_bin().replace('\\', "\\\\"),
            output = output.display().to_string().replace('\\', "\\\\"),
        ),
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success()
        .stdout("completed       success test    \n");
}

#[test]
fn a_pane_child_is_told_its_inner_geometry() {
    // A Cells pane with no border and no padding has an inner width
    // equal to its declared cells, whatever the terminal is — so the
    // assertion is exact and does not depend on the harness having a
    // tty.
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = rat_bin().replace('\\', "\\\\");
    let file = fixture(
        dir.path(),
        "geom.kdl",
        &format!(
            r#"
defaults height=3 chrome=#false border="none" padding="0" width="20"

pane "cols" {{
    command "{bin}" "__env" "RAT_WIDTH"
}}

pane "rows" {{
    command "{bin}" "__env" "RAT_HEIGHT"
}}

pane "whoami" {{
    command "{bin}" "__env" "RAT_PANE"
}}
"#
        ),
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(
        stdout.contains("20"),
        "RAT_WIDTH is the pane's cells: {stdout:?}"
    );
    assert!(
        stdout.contains('3'),
        "RAT_HEIGHT is the pane's inner rows: {stdout:?}"
    );
    assert!(
        stdout.contains("whoami"),
        "RAT_PANE is the pane's name: {stdout:?}"
    );
}

#[test]
fn a_pane_taller_than_its_box_is_truncated_keep_top() {
    // `rat style` joins multiple arguments with newlines, so this child
    // prints five lines into a three-row box.
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = rat_bin().replace('\\', "\\\\");
    let file = fixture(
        dir.path(),
        "tall.kdl",
        &format!(
            r#"
pane "tall" {{
    height 3
    chrome #false
    border "none"
    command "{bin}" "style" "AAA" "BBB" "CCC" "DDD" "EEE"
}}
"#
        ),
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(
        stdout.contains("AAA"),
        "keep-top keeps the head: {stdout:?}"
    );
    assert!(
        !stdout.contains("EEE"),
        "the pin truncated nothing: {stdout:?}"
    );
}

#[test]
fn a_pane_that_has_not_run_renders_blank_at_its_declared_size() {
    // The composed frame's row count is run-constant, so however the
    // two completions interleave, every frame written is exactly the
    // declared height. A pane that has not posted yet is blank rows,
    // never a shorter frame.
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = rat_bin().replace('\\', "\\\\");
    let file = fixture(
        dir.path(),
        "stack.kdl",
        &format!(
            r#"
defaults {{
    height 3
    chrome #false
    border "none"
}}

pane "a" {{
    command "{bin}" "style" "one"
}}

pane "b" {{
    command "{bin}" "style" "two"
}}
"#
        ),
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let rows = stdout.lines().count();
    assert!(rows >= 6, "a whole frame is 6 rows, got {rows}: {stdout:?}");
    assert_eq!(
        rows % 6,
        0,
        "every frame is exactly 6 rows; got {rows}: {stdout:?}"
    );
}

#[test]
fn an_unreadable_file_names_the_path() {
    let missing = "definitely-no-such-dashboard-xyz.kdl";
    rat()
        .args(["dashboard", missing])
        .assert()
        .code(1)
        .stderr(predicates::str::contains(missing));
}

/// A file that is not KDL fails as a parse error carrying the path —
/// there is no format selection left to point at.
#[test]
fn a_file_that_is_not_kdl_names_the_path() {
    use predicates::boolean::PredicateBooleanExt;
    // The path AND the place: the syntax error carries its line and
    // column through load's context and main's prefix. Shape only —
    // the message text between them is upstream's.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(dir.path(), "board.conf", "gap = 0\n");
    rat()
        .args(["dashboard", &file])
        // The escape-free pin below needs the profile PINNED, not
        // inferred from pipedness: an ambient CLICOLOR_FORCE outvotes
        // a piped stream, and a Windows sshd session's hidden console
        // reads as a tty. NO_COLOR beats both (detect_profile checks
        // it before anything else). Piped-earns-plain itself is
        // color.rs's unit contract, not this test's.
        .env("NO_COLOR", "1")
        .assert()
        .code(1)
        .stderr(predicates::str::contains("board.conf"))
        .stderr(predicates::str::contains("line "))
        .stderr(predicates::str::contains("column "))
        // The rustc-style snippet reaches the user: the offending
        // source line is echoed on stderr, pointed at by the span.
        .stderr(predicates::str::contains("gap = 0"))
        .stderr(predicates::str::contains("\u{1b}").not());
}

/// The failure lives in the failing pane's own box — its text, its
/// exit badge — and the dashboard around it is untouched. The height
/// pin is what makes that structural: two 5-row panes compose to
/// exactly ten rows whether they succeed or fail.
#[test]
fn a_failing_pane_shows_its_exit_code_and_the_rest_of_the_dashboard_survives() {
    let dir = tempfile::tempdir().expect("tempdir");
    let steady = dir.path().join("steady");
    std::fs::write(&steady, "steady-content").expect("seed");
    let decl = dir.path().join("dash.kdl");
    std::fs::write(
        &decl,
        format!(
            r#"
row-gap 0

defaults {{
    height 5
    border "rounded"
}}

pane "broken" {{
    command "{rat}" "__exitcode" "3" "boom-from-stderr"
}}

pane "steady" {{
    command "{rat}" "__cat" "{steady}"
}}
"#,
            rat = rat_bin().escape_default(),
            steady = steady.display().to_string().escape_default(),
        ),
    )
    .expect("write declaration");

    let assert = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", "--once", &decl.display().to_string()])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    // The failing pane's own box carries its stderr and its badge.
    assert!(stdout.contains("boom-from-stderr"), "{stdout:?}");
    assert!(stdout.contains(" · exit 3"), "{stdout:?}");
    // The neighbour rendered normally: a failure never truncates it.
    assert!(stdout.contains("steady-content"), "{stdout:?}");
    // Both declared heights intact — the whole point of the pin.
    assert_eq!(
        stdout.trim_end_matches('\n').split('\n').count(),
        10,
        "declared heights must survive a failure: {stdout:?}"
    );
    // Nothing writes outside the frame engine. The failing child's
    // stderr went into its pane, not to the terminal.
    assert_eq!(
        String::from_utf8_lossy(&assert.get_output().stderr),
        "",
        "a failing pane must not leak to the terminal"
    );
}

/// Stream the piped dashboard's stdout through a channel so waiting for
/// a frame is bounded: a blocking read cannot swallow the deadline.
/// Duplicated from the watch suite's local helpers, never lifted — that
/// file is the byte-identity witness.
fn stdout_stream(stdout: std::process::ChildStdout) -> std::sync::mpsc::Receiver<Vec<u8>> {
    use std::io::Read;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut stdout = stdout;
        let mut buf = [0u8; 4096];
        while let Ok(n) = stdout.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                return;
            }
        }
    });
    rx
}

/// `stdout_stream`'s stderr sibling: the once notice is a STDERR
/// surface, and the frame must stay off it.
fn stderr_stream(stderr: std::process::ChildStderr) -> std::sync::mpsc::Receiver<Vec<u8>> {
    use std::io::Read;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut stderr = stderr;
        let mut buf = [0u8; 4096];
        while let Ok(n) = stderr.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                return;
            }
        }
    });
    rx
}

/// Drain the stream until the needle appears — a missing frame is a
/// clean failure, never a hang.
fn read_until(stream: &std::sync::mpsc::Receiver<Vec<u8>>, seen: &mut String, needle: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if seen.contains(needle) {
            return;
        }
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(!left.is_zero(), "never saw {needle:?} in {seen:?}");
        match stream.recv_timeout(left) {
            Ok(chunk) => seen.push_str(&String::from_utf8_lossy(&chunk)),
            Err(_) => panic!("never saw {needle:?} in {seen:?}"),
        }
    }
}

/// Reap the dashboard even when an assertion panics: an orphaned child
/// holds the harness's stdout pipe open and hangs the whole run.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Per-pane triggers: a fire routes to the pane that DECLARED it. The
/// declarer is deliberately the SECOND source: a wiring that routes
/// every fire to source 0 (a shared gate, a hardcoded index) re-runs
/// alpha instead — whose bytes are unchanged, so the gated pipe writes
/// nothing and v1 never appears.
#[test]
fn a_file_trigger_refreshes_only_its_own_pane() {
    let dir = tempfile::tempdir().expect("tempdir");
    let steady = dir.path().join("steady");
    let shared = dir.path().join("shared");
    let untouched = dir.path().join("untouched");
    std::fs::write(&steady, "a0").expect("seed");
    std::fs::write(&shared, "v0").expect("seed");
    std::fs::write(&untouched, "x").expect("seed");
    let decl = dir.path().join("dash.kdl");
    std::fs::write(
        &decl,
        format!(
            r#"
row-gap 0

defaults {{
    height 1
    border "none"
    chrome #false
    interval "never"
    trigger-debounce "0ms"
}}

pane "alpha" {{
    command "{rat}" "__cat" "{steady}"
    trigger "file:{untouched}"
}}

pane "beta" {{
    command "{rat}" "__cat" "{shared}"
    trigger "file:{shared}"
}}
"#,
            rat = rat_bin().escape_default(),
            steady = steady.display().to_string().escape_default(),
            shared = shared.display().to_string().escape_default(),
            untouched = untouched.display().to_string().escape_default(),
        ),
    )
    .expect("write declaration");

    let dash = std::process::Command::new(rat_bin())
        .args(["dashboard", &decl.display().to_string()])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn rat dashboard piped");
    let mut dash = KillOnDrop(dash);
    let stream = stdout_stream(dash.0.stdout.take().expect("piped stdout"));
    let mut seen = String::new();
    read_until(&stream, &mut seen, "v0"); // both panes' first tick

    std::fs::write(&shared, "v1").expect("mtime change");
    read_until(&stream, &mut seen, "v1"); // beta's trigger-driven frame

    // The panes stack in declaration order: in the refreshed frame,
    // alpha's retained row still precedes beta's new one.
    let last_frame = seen.rfind("a0").expect("alpha's retained row");
    assert!(
        seen[last_frame..].contains("v1"),
        "the refreshed frame keeps declaration order: {seen:?}"
    );
    // KillOnDrop reaps: kill only SENDS the signal, and an unreaped
    // child zombies (unix) and races tempdir cleanup.
}

/// Every change fires, not just the first. The loop now stats the watched
/// union on its own account, to learn whether a path ever moves while the
/// dashboard is idle — and that observer keeps its OWN baselines. If it ever
/// shared them with the trigger, its stat would consume the fire and the pane
/// would quietly stop refreshing, which is the failure this guards.
#[test]
fn successive_trigger_changes_each_refresh_the_pane() {
    let dir = tempfile::tempdir().expect("tempdir");
    let watched = dir.path().join("watched");
    std::fs::write(&watched, "v0").expect("seed");
    let decl = dir.path().join("dash.kdl");
    std::fs::write(
        &decl,
        format!(
            r#"
row-gap 0

defaults {{
    height 1
    border "none"
    chrome #false
    interval "never"
    trigger-debounce "0ms"
}}

pane "only" {{
    command "{rat}" "__cat" "{watched}"
    trigger "file:{watched}"
}}
"#,
            rat = rat_bin().escape_default(),
            watched = watched.display().to_string().escape_default(),
        ),
    )
    .expect("write declaration");

    let dash = std::process::Command::new(rat_bin())
        .args(["dashboard", &decl.display().to_string()])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn rat dashboard piped");
    let mut dash = KillOnDrop(dash);
    let stream = stdout_stream(dash.0.stdout.take().expect("piped stdout"));
    let mut seen = String::new();
    read_until(&stream, &mut seen, "v0");

    // Three in a row: a shared baseline would swallow one of them.
    for value in ["v1", "v2", "v3"] {
        std::fs::write(&watched, value).expect("mtime change");
        read_until(&stream, &mut seen, value);
    }
}

/// `--once` prints ONE complete frame: a staggered pane must not make
/// the partial composition reach the pipe first.
#[test]
fn once_emits_exactly_one_complete_frame() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = rat_bin().replace('\\', "\\\\");
    let file = fixture(
        dir.path(),
        "staggered.kdl",
        &format!(
            r#"
row-gap 0

defaults {{
    height 1
    chrome #false
    border "none"
}}

pane "quick" {{
    command "{bin}" "style" "one"
}}

pane "slow" {{
    command "{bin}" "__sleep" "300" "two"
}}
"#
        ),
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert_eq!(
        stdout.trim_end_matches('\n').split('\n').count(),
        2,
        "one frame, both panes: {stdout:?}"
    );
    assert_eq!(
        stdout.matches("one").count(),
        1,
        "the quick pane printed once: {stdout:?}"
    );
    assert!(stdout.contains("two"), "the slow pane arrived: {stdout:?}");
}

/// The one deliberately slow test in this file: the notice fires only
/// after the quiet threshold (5s), and the wait IS the surface under
/// test. A test-only env knob to shrink the threshold was rejected —
/// it would ship an undocumented surface.
#[test]
fn once_says_which_pane_it_is_still_waiting_on() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("empty.log");
    std::fs::write(&log, "").expect("seed");
    let decl = dir.path().join("dash.kdl");
    std::fs::write(
        &decl,
        format!(
            r#"
defaults {{
    height 3
    border "none"
    chrome #false
}}

pane "build" {{
    command "{rat}" "__lines" "1"
}}

pane "logs" {{
    command "{rat}" "__follow" "{log}"
}}
"#,
            rat = rat_bin().escape_default(),
            log = log.display().to_string().escape_default(),
        ),
    )
    .expect("write declaration");

    let dash = std::process::Command::new(rat_bin())
        .args(["dashboard", &decl.display().to_string(), "--once"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn rat dashboard piped");
    let mut dash = KillOnDrop(dash);
    let errs = stderr_stream(dash.0.stderr.take().expect("piped stderr"));
    let outs = stdout_stream(dash.0.stdout.take().expect("piped stdout"));
    let mut seen = String::new();
    read_until(&errs, &mut seen, "\"logs\"");
    assert!(
        seen.contains("still waiting"),
        "the notice says so: {seen:?}"
    );
    assert!(
        !seen.contains("\"build\""),
        "the finished pane is never named: {seen:?}"
    );
    // No stdout byte moved: a partial wave never reaches the pipe, and
    // the diagnostic must not change that.
    assert!(
        outs.try_recv().is_err(),
        "stdout stays empty while --once waits"
    );
    // One line, once: keep draining and the notice must not repeat —
    // the loop spins in 50ms slices, so a broken one-shot would say it
    // again a dozen times over this window.
    std::thread::sleep(std::time::Duration::from_millis(600));
    while let Ok(chunk) = errs.try_recv() {
        seen.push_str(&String::from_utf8_lossy(&chunk));
    }
    assert_eq!(
        seen.matches("still waiting").count(),
        1,
        "the notice is one-shot: {seen:?}"
    );
}

#[test]
fn once_timeout_exits_124_and_writes_no_frame() {
    // An undeclared follower under a short bound: exit 124, stderr
    // names the pane, and stdout is EMPTY — a partial frame is a lie
    // in the mode whose stdout is the frame.
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("empty.log");
    std::fs::write(&log, "").expect("seed");
    let file = fixture(
        dir.path(),
        "bounded.kdl",
        &format!(
            r#"
pane "logs" {{
    height 3
    chrome #false
    border "none"
    command "{bin}" "__follow" "{log}"
}}
"#,
            bin = rat_bin().replace('\\', "\\\\"),
            log = log.display().to_string().replace('\\', "\\\\"),
        ),
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once", "--once-timeout", "300ms"])
        .assert()
        .code(124)
        .stderr(predicates::str::contains("\"logs\""))
        .stderr(predicates::str::contains("gave up"));
    assert!(
        assert.get_output().stdout.is_empty(),
        "stdout must be empty on expiry"
    );
}

#[test]
fn once_timeout_needs_once() {
    use predicates::boolean::PredicateBooleanExt;
    rat()
        .args(["dashboard", "whatever.kdl", "--once-timeout", "1s"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("--once"))
        .stderr(predicates::str::contains("unexpected argument").not());
}

#[test]
fn once_timeout_does_not_fire_when_every_pane_finishes() {
    // The no-regression pin: a slow-but-finishing pane under a
    // generous bound exits 0 with its one frame, exactly as if the
    // flag were absent.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "finishes.kdl",
        &format!(
            r#"
pane "slow" {{
    height 3
    chrome #false
    border "none"
    command "{bin}" "__sleep" "200" "done-now"
}}
"#,
            bin = rat_bin().replace('\\', "\\\\"),
        ),
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once", "--once-timeout", "30s"])
        .assert()
        .success()
        .stdout(predicates::str::contains("done-now"));
}

#[test]
fn a_dashboard_title_heads_the_once_frame() {
    // The declared title is the frame's first line, piped and --once
    // alike — the same contract watch --title has always had.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "titled.kdl",
        &format!(
            r#"
title "Deploy status"

pane "build" {{
    height 3
    chrome #false
    border "none"
    command "{bin}" "style" "one"
}}
"#,
            bin = rat_bin().replace('\\', "\\\\")
        ),
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let first = stdout.lines().next().expect("a frame");
    assert!(
        first.contains("Deploy status"),
        "the title heads the frame: {stdout:?}"
    );
    assert!(stdout.contains("one"), "the pane still renders: {stdout:?}");
}

#[test]
fn a_ref_sourced_title_renders_the_pane_not_the_fallback() {
    // Role donation end to end: the referenced pane is the visible
    // title, and the fallback text never reaches the frame — it is
    // the ROLE's fallback, not a rendered line.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "reffed.kdl",
        &format!(
            r##"
title "Fallback" ref="#header"

pane "header" {{
    height 1
    chrome #false
    border "none"
    command "{bin}" "style" "CUSTOM-HEADER"
}}

pane "body" {{
    height 3
    chrome #false
    border "none"
    command "{bin}" "style" "body-line"
}}
"##,
            bin = rat_bin().replace('\\', "\\\\")
        ),
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let first = stdout.lines().next().expect("a frame");
    assert!(
        first.contains("CUSTOM-HEADER"),
        "the pane heads the frame: {stdout:?}"
    );
    assert!(
        !stdout.contains("Fallback"),
        "the fallback is role text, never a rendered line: {stdout:?}"
    );
}

#[test]
fn a_piped_once_dashboard_never_touches_the_tab_title() {
    use predicates::boolean::PredicateBooleanExt;
    // The tab title is interactive chrome: a pipe gets frame bytes
    // and nothing else — no stack push, no OSC.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "titled.kdl",
        &format!(
            "title \"Deploy\"\n\npane \"a\" {{\n    height 3\n    chrome #false\n    border \"none\"\n    command \"{bin}\" \"style\" \"one\"\n}}\n",
            bin = rat_bin().replace('\\', "\\\\")
        ),
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\u{1b}]2;").not())
        .stdout(predicates::str::contains("\u{1b}[22;2t").not());
}

#[test]
fn a_pane_that_cannot_start_shows_the_reason_before_the_path() {
    // A declared width no terminal can widen truncates from the
    // right, so whatever sits last is what no reader sees. The
    // missing command is a LONG absolute path: the OS reason must
    // survive the budget, and the path's tail is what pays. The
    // reason is matched by `os error`, never an OS-specific sentence.
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = format!("{}-definitely-missing", rat_bin());
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            r#"
pane "plan" {{
    command "{cmd}"
    width "72"
    height 3
    chrome #false
    border "none"
    padding "0"
}}
"#,
            cmd = missing.replace('\\', "\\\\")
        ),
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .env("RAT_WIDTH", "100")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(
        stdout.contains("os error"),
        "the reason survives the width: {stdout:?}"
    );
    assert!(
        !stdout.contains("definitely-missing"),
        "the path's tail is what overflows: {stdout:?}"
    );
}

/// Piped mode honors the handed-down geometry: a nested one-shot
/// dashboard sizes itself to its pane instead of a hardcoded 80
/// columns.
#[test]
fn a_piped_dashboard_sizes_from_rat_width() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = rat_bin().replace('\\', "\\\\");
    let file = fixture(
        dir.path(),
        "sized.kdl",
        &format!(
            r#"
pane "wide" {{
    height 2
    chrome #false
    border "none"
    command "{bin}" "style" "x"
}}
"#
        ),
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .env("RAT_WIDTH", "40")
        .env("RAT_HEIGHT", "20")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    for line in stdout.trim_end_matches('\n').split('\n') {
        assert_eq!(line.chars().count(), 40, "a 40-cell frame: {line:?}");
    }
}

/// Nested layout nodes: a row holding a column beside a pane renders
/// as a grid — the engine's tree was recursive from day one, and the
/// declaration now reaches it.
#[test]
fn a_nested_layout_renders_a_grid() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = rat_bin().replace('\\', "\\\\");
    let file = fixture(
        dir.path(),
        "grid.kdl",
        &format!(
            r#"
defaults height=1 chrome=#false border="none"

row {{
    column {{
        pane "a" {{
            command "{bin}" "style" "one"
        }}
        pane "b" {{
            command "{bin}" "style" "two"
        }}
    }}
    pane "c" height=2 {{
        command "{bin}" "style" "three"
    }}
}}
"#
        ),
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .env("RAT_WIDTH", "40")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let rows: Vec<&str> = stdout.trim_end_matches('\n').split('\n').collect();
    assert_eq!(rows.len(), 2, "a 2-row grid: {stdout:?}");
    assert!(
        rows[0].contains("one") && rows[0].contains("three"),
        "top of the column beside the tall pane: {stdout:?}"
    );
    assert!(
        rows[1].contains("two"),
        "bottom of the column on the second grid row: {stdout:?}"
    );
}

#[test]
fn a_flooding_pane_wears_the_marker_on_its_own_chrome_row() {
    // The pane route's own end-to-end proof. The unit tests show the
    // badge renders and joins the signature; only this shows the count
    // reaching it from a real child, through the reader, the outcome
    // and the drain. A shared decision covered on one route only is
    // covered on neither.
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = rat_bin().replace('\\', "\\\\");
    let file = fixture(
        dir.path(),
        "flood.kdl",
        &format!(
            r#"
defaults height=6 width="60" border="none" padding="0"

pane "flood" {{
    command "{bin}" "__lines" "1500"
}}

pane "quiet" {{
    command "{bin}" "style" "calm"
}}
"#
        ),
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    assert!(
        stdout.contains("500 lines dropped"),
        "the flooding pane must say so; got {stdout:?}"
    );
    // The default overflow keeps the head, so the pane retained lines
    // 0..999 and paints the first of them. The last line the child
    // printed is gone — the direction working, not a fault.
    assert!(
        stdout.starts_with('0'),
        "a keep-top pane shows its head: {stdout:?}"
    );
    assert!(
        !stdout.contains("1499"),
        "and its tail is what went: {stdout:?}"
    );
    // One pane overflowed, not both: the marker is per-pane state and
    // the quiet pane's chrome row must stay clean.
    assert_eq!(
        stdout.matches("lines dropped").count(),
        1,
        "only the flooding pane wears it: {stdout:?}"
    );
}

/// The piped route for a live pane under `--once`: a source whose child
/// never exits still produces exactly one frame and a clean exit,
/// because its first emission is what satisfies the once condition.
///
/// Its own test rather than a corollary of the pty one: a piped run and
/// a terminal run take different branches through the loop, and the
/// shared half is the half already covered.
#[test]
#[cfg(unix)]
fn once_with_a_live_pane_emits_one_frame_and_exits() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("log");
    std::fs::write(&log, "piped-live-content\n").expect("seed the log");
    let file = fixture(
        dir.path(),
        "live.kdl",
        &format!(
            r#"
defaults {{
    height 3
    chrome #false
}}

pane "follower" live=#true {{
    command "{bin}" "__follow" "{log}"
}}
"#,
            bin = rat_bin().replace('\\', "\\\\"),
            log = log.display()
        ),
    );
    let out = rat()
        .args(["dashboard", "--once", &file])
        .env("RAT_WIDTH", "40")
        .env("RAT_HEIGHT", "10")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    assert!(
        stdout.contains("piped-live-content"),
        "the live pane's body never reached the frame: {stdout:?}"
    );
    assert_eq!(
        stdout.matches("piped-live-content").count(),
        1,
        "--once must emit exactly one frame: {stdout:?}"
    );
}

/// The no-shebang fallback, through a NAMED shell — and the tmpdir
/// stays untouched: a body with no `#!` is never materialized.
#[cfg(unix)]
#[test]
fn a_script_body_with_no_shebang_falls_back_to_the_shell() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tmp = tempfile::tempdir().expect("isolated TMPDIR");
    let file = fixture(
        dir.path(),
        "board.kdl",
        "defaults height=3 chrome=#false\npane \"fall\" {\n    shell \"sh\"\n    script \"echo fell-back\"\n}\n",
    );
    rat()
        .env("NO_COLOR", "1")
        .env("TMPDIR", tmp.path())
        .args(["dashboard", &file, "--once"])
        .assert()
        .success()
        .stdout(predicates::str::contains("fell-back"));
    let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
        .expect("read the isolated tmpdir")
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("rat-script.")
        })
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

/// The no-shebang fallback with NO shell key anywhere: absence promotes
/// to the platform's shell — the same thing `shell #true` means — so a
/// plain body just works on every platform. The body invokes the rat
/// binary, this file's portable-child convention.
#[test]
fn a_script_body_with_no_shell_key_falls_back_to_the_platform_shell() {
    let dir = tempfile::tempdir().expect("tempdir");
    // cmd wants backslash separators for a program path. The body is
    // unquoted — the quoted spelling has its own windows arm
    // (`a_quoted_program_in_a_script_body_runs`).
    let bin = if cfg!(windows) {
        rat_bin().replace('/', "\\")
    } else {
        rat_bin()
    };
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            "defaults height=3 chrome=#false\npane \"fall\" {{\n    script \"{bin} style promoted-shell\"\n}}\n",
            bin = bin.replace('\\', "\\\\")
        ),
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success()
        .stdout(predicates::str::contains("promoted-shell"));
}

/// A script body that QUOTES its program path, routed through the
/// promoted platform shell — the spelling an author with a spaced path
/// must write. The body has to reach `cmd /C` byte-for-byte: cmd does
/// not parse the `\"` escapes MSVCRT-style argument quoting writes.
#[cfg(windows)]
#[test]
fn a_quoted_program_in_a_script_body_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let body = format!(
        "\"{}\" style quoted-script-ran",
        rat_bin().replace('/', "\\")
    );
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            "defaults height=3 chrome=#false\npane \"quoted\" {{\n    script \"{body}\"\n}}\n",
            body = body.replace('\\', "\\\\").replace('"', "\\\"")
        ),
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success()
        .stdout(predicates::str::contains("quoted-script-ran"));
}

/// The three winvm-measured mechanisms at once: any one missing fails
/// this — an unstripped `#!` (cmd chokes on the line), a missing `.cmd`
/// extension ("is not recognized"), or a missing `cmd /C` (the file is
/// not a PE).
#[cfg(windows)]
#[test]
fn a_cmd_body_runs_as_a_batch_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        "defaults height=4 chrome=#false\npane \"batch\" {\n    script \"#!cmd\\n@echo off\\nset /a x=6*7\\necho answer-%x%\"\n}\n",
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success()
        .stdout(predicates::str::contains("answer-42"));
}

/// `.ps1` (pwsh refuses anything else), `env` substitution, and
/// `-NoProfile` WITHOUT `-Command` (with `-Command` the path would be
/// evaluated as an expression, not run as a file) — measured working
/// form: `pwsh -NoProfile <file.ps1>`.
#[cfg(windows)]
#[test]
fn a_pwsh_body_runs_as_a_ps1_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        "defaults height=4 chrome=#false\npane \"ps\" {\n    script \"#!/usr/bin/env pwsh\\nWrite-Output (6 * 7)\"\n}\n",
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success()
        .stdout(predicates::str::contains("42"));
}

/// The standard-spawn-does-the-search contract: `env powershell`
/// resolves the name on PATH (Rust Command's walk with `.exe`
/// inference; no PATHEXT), and Windows PowerShell answers.
#[cfg(windows)]
#[test]
fn an_env_shebang_finds_its_interpreter_on_the_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        "defaults height=4 chrome=#false\npane \"onpath\" {\n    script \"#!/usr/bin/env powershell\\nWrite-Output found-on-path\"\n}\n",
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success()
        .stdout(predicates::str::contains("found-on-path"));
}

#[test]
fn an_unknown_variable_names_itself_and_the_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        "variables {\n    plan \"/tmp/x\"\n}\n\npane \"a\" {\n    command \"true\"\n    height 3\n    title \"{{plna}}\"\n}\n",
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("unknown variable `plna`"))
        .stderr(predicates::str::contains("declared variables are plan"))
        .stderr(predicates::str::contains("board.kdl"));
}

#[test]
fn a_variable_override_reaches_the_board() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            r#"
variables {{
    word "hello"
}}

defaults {{ height 3
chrome #false }}

pane "a" {{
    command "{bin}" "style" "{{{{word}}}}"
}}
"#,
            bin = rat_bin().replace('\\', "\\\\")
        ),
    );
    // Nothing expands yet (INV-2 — expansion comes with the spawn-time
    // phase), so this route asserts the FLAG's reach: the board loads,
    // and an override naming an undeclared variable refuses by name.
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once", "-v", "word=goodbye"])
        .assert()
        .success();
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once", "-v", "wrod=goodbye"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "the board declares no variable `wrod`",
        ));
}

#[test]
fn a_constant_never_executes_anything() {
    // A variable with NO `shell`, whose VALUE is command-shaped. The
    // classifier must read `VarSource`, never "looks like a command".
    let dir = tempfile::tempdir().expect("tempdir");
    let counter = dir.path().join("runs");
    #[cfg(unix)]
    let shaped = format!("printf x >> {}", counter.display());
    #[cfg(windows)]
    let shaped = format!("echo x>> \"{}\"", counter.display());
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            "variables {{\n    probe \"{shaped}\"\n}}\n\npane \"a\" {{\n    command \"{bin}\" \"style\" \"ok\"\n    height 3\n    chrome #false\n}}\n",
            shaped = shaped.replace('\\', "\\\\").replace('"', "\\\""),
            bin = rat_bin().replace('\\', "\\\\"),
        ),
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    assert!(!counter.exists(), "a constant never spawns anything");
}

#[cfg(unix)]
#[test]
fn defaults_shell_does_not_reach_a_variable() {
    // Two boards, byte-identical except that one declares
    // `defaults { shell "fish" }`. The variable derives under ITS OWN
    // shell either way — variables are the board's distribution
    // surface, and a shipped template's values must not change dialect
    // because the importing board declares a different `defaults`.
    // The probe writes the running shell's $0, which differs by
    // dialect; the unconditional half is that the two boards agree.
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = rat_bin().replace('\\', "\\\\");
    let mut outputs = Vec::new();
    for (name, defaults_line) in [("plain.kdl", ""), ("fishy.kdl", "shell \"fish\"\n")] {
        let probe = dir.path().join(format!("{name}.probe"));
        let file = fixture(
            dir.path(),
            name,
            &format!(
                "variables {{\n    probe \"echo $0 >> {path}; echo ok\" shell=#true\n}}\n\ndefaults {{\n    height 3\n    chrome #false\n    {defaults_line}}}\n\npane \"a\" {{\n    command \"{bin}\" \"style\" \"ok\"\n    shell #false\n}}\n",
                path = probe.display(),
            ),
        );
        rat()
            .env("NO_COLOR", "1")
            .args(["dashboard", &file, "--once"])
            .assert()
            .success();
        outputs.push(std::fs::read_to_string(&probe).expect("the probe ran"));
    }
    assert_eq!(outputs[0], outputs[1], "defaults leaked into a variable");
}

#[test]
fn a_command_variable_may_reference_another_variable_in_either_order() {
    // The unordered walk (INV-3) meets the runner end-to-end: the
    // command TEXT is expanded against its dependencies before it
    // runs, wherever the dependencies are declared — including a
    // three-hop chain declared entirely backwards.
    let dir = tempfile::tempdir().expect("tempdir");
    let out_one = dir.path().join("one");
    let out_two = dir.path().join("two");
    #[cfg(unix)]
    let (write_one, write_two) = (
        format!(
            "printf %s {{{{root}}}}/store >> {}; printf ok",
            out_one.display()
        ),
        format!("printf %s {{{{c}}}} >> {}; printf ok", out_two.display()),
    );
    #[cfg(windows)]
    let (write_one, write_two) = (
        format!(
            "echo {{{{root}}}}/store>> \"{}\" & echo ok",
            out_one.display()
        ),
        format!("echo {{{{c}}}}>> \"{}\" & echo ok", out_two.display()),
    );
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            "variables {{\n    store \"{w1}\" shell=#true\n    root \"/tmp/xyz\"\n    final \"{w2}\" shell=#true\n    c \"{{{{b}}}}/c\"\n    b \"{{{{a}}}}/b\"\n    a \"root\"\n}}\n\npane \"a\" {{\n    command \"{bin}\" \"style\" \"ok\"\n    height 3\n    chrome #false\n}}\n",
            w1 = write_one.replace('\\', "\\\\").replace('"', "\\\""),
            w2 = write_two.replace('\\', "\\\\").replace('"', "\\\""),
            bin = rat_bin().replace('\\', "\\\\"),
        ),
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let one = std::fs::read_to_string(&out_one).expect("store derived");
    assert_eq!(one.trim(), "/tmp/xyz/store");
    let two = std::fs::read_to_string(&out_two).expect("final derived");
    assert_eq!(two.trim(), "root/b/c");
}

#[test]
fn an_override_suppresses_that_names_command_entirely() {
    // INV-4.4: `-v store=…` means store's command NEVER runs — and a
    // sibling that was not overridden still does.
    let dir = tempfile::tempdir().expect("tempdir");
    let store_counter = dir.path().join("store-runs");
    let other_counter = dir.path().join("other-runs");
    #[cfg(unix)]
    let (store_cmd, other_cmd) = (
        format!("printf x >> {}; printf a", store_counter.display()),
        format!("printf y >> {}; printf b", other_counter.display()),
    );
    #[cfg(windows)]
    let (store_cmd, other_cmd) = (
        format!("echo x>> \"{}\" & echo a", store_counter.display()),
        format!("echo y>> \"{}\" & echo b", other_counter.display()),
    );
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            "variables {{\n    store \"{s}\" shell=#true\n    other \"{o}\" shell=#true\n}}\n\npane \"a\" {{\n    command \"{bin}\" \"style\" \"ok\"\n    height 3\n    chrome #false\n}}\n",
            s = store_cmd.replace('\\', "\\\\").replace('"', "\\\""),
            o = other_cmd.replace('\\', "\\\\").replace('"', "\\\""),
            bin = rat_bin().replace('\\', "\\\\"),
        ),
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once", "-v", "store=/given"])
        .assert()
        .success();
    assert!(!store_counter.exists(), "the overridden command never ran");
    let other = std::fs::read_to_string(&other_counter).expect("the sibling ran");
    assert_eq!(other.matches('y').count(), 1);
}

// The memoization witness lives in tests/pty_dashboard.rs
// (`a_once_at_load_variable_expands_to_the_same_bytes_on_every_spawn`):
// several spawns are a frame-loop affair, and the pty suite is where
// respawns are drivable.

#[test]
fn each_failure_refuses_the_board_and_writes_no_frame() {
    // One board per failure mode. Each refuses end-to-end: non-zero
    // exit, NO frame on stdout (a partial dashboard is worse than a
    // refusal), and stderr naming both the variable and the file.
    let dir = tempfile::tempdir().expect("tempdir");
    #[cfg(unix)]
    let hang = "sleep 7";
    #[cfg(windows)]
    let hang = "ping -n 8 127.0.0.1 > NUL";
    let boards = [
        (
            "exits.kdl",
            "bad",
            "variables {\n    bad \"exit 3\" shell=#true\n}\n".to_string(),
        ),
        (
            "empty.kdl",
            "quiet",
            "variables {\n    quiet \"cd .\" shell=#true\n}\n".to_string(),
        ),
        (
            "spawnless.kdl",
            "lost",
            "variables {\n    lost \"cd .\" shell=\"definitely-not-a-shell-xyz\"\n}\n".to_string(),
        ),
        (
            "hangs.kdl",
            "slow",
            format!("variables {{\n    slow \"{hang}\" shell=#true\n}}\n"),
        ),
    ];
    for (name, variable, variables) in boards {
        let file = fixture(
            dir.path(),
            name,
            &format!(
                "{variables}\npane \"a\" {{\n    command \"{bin}\" \"style\" \"ok\"\n    height 3\n    chrome #false\n}}\n",
                bin = rat_bin().replace('\\', "\\\\"),
            ),
        );
        rat()
            .env("NO_COLOR", "1")
            .args(["dashboard", &file, "--once"])
            .assert()
            .failure()
            .stdout(predicates::str::is_empty())
            .stderr(predicates::str::contains(format!(
                "variable \"{variable}\""
            )))
            .stderr(predicates::str::contains(name));
    }
}

#[test]
fn the_load_pass_still_refuses_a_failing_once_at_load_variable() {
    // The two tiers must not have merged into one lenient path: a
    // failing NON-deferred command variable still refuses at load,
    // however healthy the deferred one beside it is.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            "variables {{\n    bad \"exit 3\" shell=#true\n    fine \"cd .\" shell=#true defer=#true\n}}\n\npane \"a\" {{\n    command \"{bin}\" \"style\" \"ok\"\n    height 3\n    chrome #false\n}}\n",
            bin = rat_bin().replace('\\', "\\\\"),
        ),
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("variable \"bad\""));
}

#[test]
fn a_command_argv_expands_at_spawn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            "variables {{\n    msg \"one two\"\n}}\n\npane \"p\" {{\n    height 2\n    chrome #false\n    border \"none\"\n    command \"{bin}\" \"style\" \"{{{{msg}}}}\"\n}}\n",
            bin = rat_bin().replace('\\', "\\\\"),
        ),
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    // `rat style` joins multiple arguments with NEWLINES, so a re-split
    // would show up as two rows — the decisive argv-boundary assertion.
    assert!(
        stdout.lines().any(|line| line.contains("one two")),
        "the expansion reached the child on one row: {stdout}"
    );
    assert!(
        !stdout.lines().any(|line| line.trim() == "one"),
        "a re-split would put `one` on its own row: {stdout}"
    );
    assert!(!stdout.contains("{{msg}}"), "{stdout}");
}

#[test]
fn a_whole_command_line_in_one_element_names_one_program() {
    // The expansion lands INSIDE argv[0] and is never re-split: a split
    // would make argv[0] the path's first half and the spawn would
    // fail.
    let dir = tempfile::tempdir().expect("tempdir");
    #[cfg(windows)]
    let spaced = dir.path().join("two words.exe");
    #[cfg(not(windows))]
    let spaced = dir.path().join("two words");
    std::fs::copy(rat_bin(), &spaced).expect("copy the binary");
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            "variables {{\n    prog \"{prog}\"\n}}\n\npane \"p\" {{\n    height 2\n    chrome #false\n    border \"none\"\n    command \"{{{{prog}}}}\" \"style\" \"hi\"\n}}\n",
            prog = spaced.display().to_string().replace('\\', "\\\\"),
        ),
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(stdout.contains("hi"), "{stdout}");
    assert!(!stdout.contains("os error"), "{stdout}");
}

#[test]
fn the_same_content_under_shell_runs_as_a_command_line() {
    // The other half of the argv-boundary rule: a board that wants a
    // whole command line uses `shell`.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            "variables {{\n    cmd \"{bin} style hi\"\n}}\n\npane \"p\" {{\n    height 2\n    chrome #false\n    border \"none\"\n    shell #true\n    command \"{{{{cmd}}}}\"\n}}\n",
            bin = rat_bin().replace('\\', "\\\\"),
        ),
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(stdout.contains("hi"), "{stdout}");
}

/// The reference sits in a command the body's head never reaches, and
/// the platform shell spells "then run this too" its own way: `sh -c`
/// takes a newline, while `cmd /C` takes ONE command line — a second
/// LINE handed to cmd is simply never run — so cmd sequences with `&`.
#[test]
fn a_script_body_expands_at_spawn() {
    let dir = tempfile::tempdir().expect("tempdir");
    #[cfg(unix)]
    let then = "\\n";
    #[cfg(windows)]
    let then = " & ";
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            "variables {{\n    msg \"expanded-at-spawn\"\n}}\n\npane \"p\" {{\n    height 2\n    chrome #false\n    border \"none\"\n    shell #true\n    script \"echo start{then}echo {{{{msg}}}}\"\n}}\n"
        ),
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(stdout.contains("expanded-at-spawn"), "{stdout}");
    assert!(!stdout.contains("{{msg}}"), "{stdout}");
}

/// The byte-identity witness: green BEFORE the spawn-expansion change
/// and green after — a witness that has never been green proves
/// nothing. A variable-free board must take literally the same path it
/// always took.
#[test]
fn a_board_with_no_variables_renders_byte_identically() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            "pane \"w\" {{\n    command \"{bin}\" \"style\" \"witness\"\n    height 3\n    chrome #false\n    border \"none\"\n}}\n",
            bin = rat_bin().replace('\\', "\\\\"),
        ),
    );
    // The width is an INPUT, and the witness pins it: a piped frame is
    // composed for whoever reads the pipe, so it takes RAT_WIDTH over
    // any console it can still measure. Left unpinned the literal is
    // only literal where the runner happens to agree — a piped unix run
    // measures nothing and falls back to 80, while Windows CI reports
    // its own console and renders 120.
    let assert = rat()
        .env("NO_COLOR", "1")
        .env("RAT_WIDTH", "80")
        .env("RAT_HEIGHT", "24")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let expected = format!("{:<80}\n{:<80}\n{:<80}\n", "witness", "", "");
    assert_eq!(stdout, expected, "the frame's exact bytes moved");
}

#[test]
fn a_deferred_failure_renders_in_that_panes_box_and_leaves_the_board_alone() {
    // A spawn failure is FRAME CONTENT, not an exit code: the failing
    // pane's box carries the teaching text naming the variable, no
    // exit badge appears (asserted via the badge's own `· exit `
    // spelling, which the quoted command cannot produce), and the
    // healthy pane is untouched.
    let dir = tempfile::tempdir().expect("tempdir");
    #[cfg(unix)]
    let failing = "false";
    #[cfg(windows)]
    let failing = "exit 1";
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            "variables {{\n    head \"{failing}\" shell=#true defer=#true\n}}\n\npane \"left\" {{\n    height 4\n    command \"{bin}\" \"style\" \"{{{{head}}}}\"\n}}\n\npane \"right\" {{\n    height 4\n    command \"{bin}\" \"style\" \"right-ok\"\n}}\n",
            bin = rat_bin().replace('\\', "\\\\"),
        ),
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(stdout.contains(r#"variable "head""#), "{stdout}");
    assert!(
        stdout.contains("right-ok"),
        "the rest of the board is untouched: {stdout}"
    );
    assert!(
        !stdout.contains("· exit "),
        "a spawn error carries no badge: {stdout}"
    );
    // The composed line's FRAME (watch's own format), not the sentence
    // (2.2's, improvable): the pane label leads the body. The trailing
    // `: {program:?}` half of the frame is NOT assertable here — the
    // teaching text is longer than the pane and the box clips it — and
    // it stays pinned by the existing spawn-error tests over
    // `pane_spawn_error_text` itself.
    assert!(
        stdout.contains(r#"left: variable "head""#),
        "the frame leads with the pane label: {stdout}"
    );
}

#[test]
fn a_raw_string_holding_a_deferred_reference_runs_nothing_at_spawn() {
    // The refs-not-text rule end to end: a raw argument's record says
    // it references nothing, so no subprocess is spawned for it and
    // the literal braces reach the child.
    let dir = tempfile::tempdir().expect("tempdir");
    let counter = dir.path().join("runs");
    #[cfg(unix)]
    let counting = format!("printf x >> {}", counter.display());
    #[cfg(windows)]
    let counting = format!("echo x>> \"{}\"", counter.display());
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            "variables {{\n    head \"{counting}\" shell=#true defer=#true\n}}\n\npane \"p\" {{\n    height 2\n    chrome #false\n    border \"none\"\n    command \"{bin}\" \"style\" #\"{{{{head}}}}\"#\n}}\n",
            counting = counting.replace('\\', "\\\\").replace('"', "\\\""),
            bin = rat_bin().replace('\\', "\\\\"),
        ),
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(
        stdout.contains("{{head}}"),
        "the braces reach the child: {stdout}"
    );
    assert!(!counter.exists(), "no derivation ran for a raw string");
}

#[test]
fn an_override_supplies_a_deferred_value_without_changing_its_tier() {
    let dir = tempfile::tempdir().expect("tempdir");
    let counter = dir.path().join("runs");
    #[cfg(unix)]
    let counting = format!("printf x >> {}", counter.display());
    #[cfg(windows)]
    let counting = format!("echo x>> \"{}\"", counter.display());
    let board = format!(
        "variables {{\n    head \"{counting}\" shell=#true defer=#true\n}}\n\n",
        counting = counting.replace('\\', "\\\\").replace('"', "\\\""),
    );
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            "{board}pane \"p\" {{\n    height 2\n    chrome #false\n    border \"none\"\n    command \"{bin}\" \"style\" \"{{{{head}}}}\"\n}}\n",
            bin = rat_bin().replace('\\', "\\\\"),
        ),
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once", "-v", "head=abc"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(
        stdout.contains("abc"),
        "the override reached the frame: {stdout}"
    );
    assert!(!counter.exists(), "the overridden command never ran");

    // The tier half: an override supplies a VALUE, never a tier, so
    // the same variable at a load-time site still refuses.
    let sited = fixture(
        dir.path(),
        "sited.kdl",
        &format!(
            "{board}pane \"p\" {{\n    height 2\n    command \"true\"\n    interval \"{{{{head}}}}\"\n}}\n"
        ),
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &sited, "--once", "-v", "head=5s"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "is read when the dashboard loads",
        ));
}

// ─── Load-time site expansion ───────────────────────────────────────

#[test]
fn a_trigger_expands_at_load() {
    // The headline route: a watcher registered on the LITERAL
    // `{{dir}}/shared` never moves, so only expansion at load can make
    // the second read succeed.
    let dir = tempfile::tempdir().expect("tempdir");
    let shared = dir.path().join("shared");
    std::fs::write(&shared, "v0").expect("seed");
    let decl = dir.path().join("dash.kdl");
    std::fs::write(
        &decl,
        format!(
            "variables {{\n    dir \"{d}\"\n}}\n\ndefaults {{\n    height 1\n    border \"none\"\n    chrome #false\n    interval \"never\"\n    trigger-debounce \"0ms\"\n}}\n\npane \"beta\" {{\n    command \"{rat}\" \"__cat\" \"{shared}\"\n    trigger \"file:{{{{dir}}}}/shared\"\n}}\n",
            d = dir.path().display().to_string().escape_default(),
            rat = rat_bin().escape_default(),
            shared = shared.display().to_string().escape_default(),
        ),
    )
    .expect("write declaration");
    let dash = std::process::Command::new(rat_bin())
        .args(["dashboard", &decl.display().to_string()])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn rat dashboard piped");
    let mut dash = KillOnDrop(dash);
    let stream = stdout_stream(dash.0.stdout.take().expect("piped stdout"));
    let mut seen = String::new();
    read_until(&stream, &mut seen, "v0");
    std::fs::write(&shared, "v1").expect("mtime change");
    read_until(&stream, &mut seen, "v1");
}

#[test]
fn an_interval_expands_at_load() {
    // Pre-fix this board is REFUSED: parse_interval("{{iv}}") fails.
    // Post-fix it loads and the pane re-runs at the expanded cadence.
    let dir = tempfile::tempdir().expect("tempdir");
    let watched = dir.path().join("watched");
    std::fs::write(&watched, "v0").expect("seed");
    let decl = dir.path().join("dash.kdl");
    std::fs::write(
        &decl,
        format!(
            "variables {{\n    iv \"50ms\"\n}}\n\npane \"p\" {{\n    height 1\n    border \"none\"\n    chrome #false\n    interval \"{{{{iv}}}}\"\n    command \"{rat}\" \"__cat\" \"{watched}\"\n}}\n",
            rat = rat_bin().escape_default(),
            watched = watched.display().to_string().escape_default(),
        ),
    )
    .expect("write declaration");
    let dash = std::process::Command::new(rat_bin())
        .args(["dashboard", &decl.display().to_string()])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn rat dashboard piped");
    let mut dash = KillOnDrop(dash);
    let stream = stdout_stream(dash.0.stdout.take().expect("piped stdout"));
    let mut seen = String::new();
    read_until(&stream, &mut seen, "v0");
    std::fs::write(&watched, "v1").expect("rewrite");
    // Only an interval-driven re-run can pick this up: no trigger is
    // declared.
    read_until(&stream, &mut seen, "v1");
}

#[test]
fn a_trigger_debounce_expands_at_load() {
    // The second parse_interval call site is a separate line, and this
    // schema's history is that a rule holds on one spelling and not
    // the other.
    let dir = tempfile::tempdir().expect("tempdir");
    let shared = dir.path().join("shared");
    std::fs::write(&shared, "v0").expect("seed");
    let decl = dir.path().join("dash.kdl");
    std::fs::write(
        &decl,
        format!(
            "variables {{\n    db \"0ms\"\n}}\n\npane \"p\" {{\n    height 1\n    border \"none\"\n    chrome #false\n    interval \"never\"\n    trigger-debounce \"{{{{db}}}}\"\n    command \"{rat}\" \"__cat\" \"{shared}\"\n    trigger \"file:{shared}\"\n}}\n",
            rat = rat_bin().escape_default(),
            shared = shared.display().to_string().escape_default(),
        ),
    )
    .expect("write declaration");
    let dash = std::process::Command::new(rat_bin())
        .args(["dashboard", &decl.display().to_string()])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn rat dashboard piped");
    let mut dash = KillOnDrop(dash);
    let stream = stdout_stream(dash.0.stdout.take().expect("piped stdout"));
    let mut seen = String::new();
    read_until(&stream, &mut seen, "v0");
    std::fs::write(&shared, "v1").expect("mtime change");
    read_until(&stream, &mut seen, "v1");
}

#[test]
fn geometry_expands_at_load() {
    // width, border, and padding all parse EXPANDED text; pre-fix the
    // board refuses at parse_width / parse_border / parse_sides.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            "variables {{\n    w \"24\"\n    b \"rounded\"\n    p \"0 2\"\n}}\n\npane \"p\" {{\n    height 3\n    chrome #false\n    width \"{{{{w}}}}\"\n    border \"{{{{b}}}}\"\n    padding \"{{{{p}}}}\"\n    command \"{bin}\" \"style\" \"boxed\"\n}}\n",
            bin = rat_bin().replace('\\', "\\\\"),
        ),
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let top = stdout
        .lines()
        .find(|line| line.trim_start().starts_with('╭'))
        .unwrap_or_else(|| panic!("no rounded border rendered: {stdout}"));
    assert_eq!(
        top.trim_end().chars().count(),
        24,
        "the box is the expanded width: {stdout}"
    );
}

#[test]
fn a_pane_title_and_the_dashboard_title_expand_at_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            "variables {{\n    t \"Pane-Label\"\n}}\ntitle \"board {{{{t}}}}\"\n\npane \"p\" {{\n    height 5\n    border \"rounded\"\n    title \"{{{{t}}}}\"\n    command \"{bin}\" \"style\" \"body\"\n}}\n",
            bin = rat_bin().replace('\\', "\\\\"),
        ),
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(stdout.contains("Pane-Label"), "{stdout}");
    assert!(!stdout.contains("{{t}}"), "{stdout}");

    // A `ref` fragment is an ID, never substituted: the unknown-ref
    // error names the WRITTEN fragment even when a variable shares its
    // name.
    let sited = fixture(
        dir.path(),
        "ref.kdl",
        &format!(
            "variables {{\n    nope \"p\"\n}}\ntitle ref=\"#nope\"\n\npane \"p\" {{\n    height 3\n    command \"{bin}\" \"style\" \"x\"\n}}\n",
            bin = rat_bin().replace('\\', "\\\\"),
        ),
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &sited, "--once"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("#nope"));
}

/// Classification agreement over template text: green since spawn-time
/// expansion landed — a regression pin, not a Red.
#[cfg(unix)]
#[test]
fn a_shebang_body_with_later_templates_still_classifies_as_a_script() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        "variables {\n    msg \"expanded-later\"\n}\n\npane \"p\" {\n    height 3\n    chrome #false\n    border \"none\"\n    script \"#!/bin/sh\\necho from-sh\\necho {{msg}}\"\n}\n",
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(stdout.contains("from-sh"), "{stdout}");
    assert!(stdout.contains("expanded-later"), "{stdout}");
}

/// The acceptance test for the plan's original motivation: a board
/// whose trigger is derived from `git rev-parse --git-common-dir`
/// resolves to the COMMON dir in a linked worktree, where `.git` is a
/// file — so the board is a distributable artifact, not a per-machine
/// hand edit. The pane's `command` half is already green (spawn-time
/// expansion); the trigger is the load half under test, so pre-fix the
/// FIRST read succeeds and the SECOND times out.
#[cfg(unix)]
#[test]
fn the_q2_linked_worktree_board_resolves_its_trigger() {
    let dir = tempfile::tempdir().expect("tempdir");
    let git = |args: &[&str], cwd: &std::path::Path| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {:?}", out);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let primary = dir.path().join("primary");
    std::fs::create_dir(&primary).expect("mkdir");
    git(&["init", "-q"], &primary);
    git(&["config", "user.name", "t"], &primary);
    git(&["config", "user.email", "t@example.invalid"], &primary);
    std::fs::write(primary.join("seed"), "s").expect("seed file");
    git(&["add", "."], &primary);
    git(&["commit", "-q", "-m", "seed"], &primary);
    git(&["worktree", "add", "-q", "../linked"], &primary);
    let linked = dir.path().join("linked");
    assert!(
        linked.join(".git").is_file(),
        ".git is a FILE in a linked worktree — the whole point"
    );
    // The independently-derived truth, canonicalized (macOS /var →
    // /private/var), proving the store resolves OUTSIDE linked/.
    let common = std::path::PathBuf::from(git(&["rev-parse", "--git-common-dir"], &linked));
    let common = std::fs::canonicalize(&common).expect("canonicalize");
    assert!(
        !common.starts_with(std::fs::canonicalize(&linked).expect("canonicalize")),
        "the common dir lives outside the linked worktree: {common:?}"
    );
    let store = common.join("pointbreak");
    std::fs::create_dir_all(&store).expect("store");
    let events = store.join("events");
    std::fs::write(&events, "seeded").expect("seed events");

    let decl = linked.join("board.kdl");
    std::fs::write(
        &decl,
        format!(
            "variables {{\n    store \"git rev-parse --git-common-dir\" shell=#true\n    events \"{{{{store}}}}/pointbreak/events\"\n}}\n\npane \"header\" {{\n    height 1\n    border \"none\"\n    chrome #false\n    interval \"never\"\n    trigger-debounce \"0ms\"\n    command \"{rat}\" \"__cat\" \"{{{{events}}}}\"\n    trigger \"file:{{{{events}}}}\"\n}}\n",
            rat = rat_bin().escape_default(),
        ),
    )
    .expect("write declaration");
    let dash = std::process::Command::new(rat_bin())
        .args(["dashboard", "board.kdl"])
        .current_dir(&linked)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn rat dashboard piped");
    let mut dash = KillOnDrop(dash);
    let stream = stdout_stream(dash.0.stdout.take().expect("piped stdout"));
    let mut seen = String::new();
    read_until(&stream, &mut seen, "seeded");
    std::fs::write(&events, "review-landed").expect("the writer moves the store");
    read_until(&stream, &mut seen, "review-landed");
}

#[test]
fn the_three_routes_expand_identically() {
    // Parity, portable legs: a once-at-load COMMAND variable in the
    // pane's command (a spawn site) and a constant in its title (a
    // load site), one board, two routes here and the live third in
    // tests/pty_dashboard.rs (`the_live_route_expands_like_the_piped_
    // ones` — the shared needles must not drift apart). The CONTENT
    // assertion is what makes this a test: three routes agreeing on
    // the wrong bytes would satisfy equality alone.
    let dir = tempfile::tempdir().expect("tempdir");
    let board = format!(
        "variables {{\n    v \"echo parity-value\" shell=#true\n    t \"Title-X\"\n}}\n\npane \"p\" {{\n    height 5\n    border \"rounded\"\n    title \"{{{{t}}}}\"\n    command \"{bin}\" \"style\" \"{{{{v}}}}\"\n}}\n",
        bin = rat_bin().replace('\\', "\\\\"),
    );
    let file = fixture(dir.path(), "board.kdl", &board);
    let once = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let once_out = String::from_utf8_lossy(&once.get_output().stdout).into_owned();
    assert!(once_out.contains("parity-value"), "{once_out}");
    assert!(once_out.contains("Title-X"), "{once_out}");

    let dash = std::process::Command::new(rat_bin())
        .env("NO_COLOR", "1")
        .args(["dashboard", &file])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn rat dashboard piped");
    let mut dash = KillOnDrop(dash);
    let stream = stdout_stream(dash.0.stdout.take().expect("piped stdout"));
    let mut seen = String::new();
    read_until(&stream, &mut seen, "parity-value");
    read_until(&stream, &mut seen, "Title-X");
    let row_of = |text: &str, needle: &str| -> String {
        text.lines()
            .find(|line| line.contains(needle))
            .unwrap_or_default()
            .trim_end()
            .to_string()
    };
    assert_eq!(
        row_of(&once_out, "parity-value"),
        row_of(&seen, "parity-value"),
        "the two piped routes agree byte for byte"
    );
    assert_eq!(row_of(&once_out, "Title-X"), row_of(&seen, "Title-X"));
}

#[cfg(unix)]
#[test]
fn a_shell_dialect_name_expands_at_load() {
    // Only a SHELL turns `|` into a pipeline: a direct spawn of a
    // program named `echo hi | tr i X` fails, and a dialect left as
    // the literal `{{sh}}` names no program at all.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        "variables {\n    sh \"sh\"\n}\n\npane \"p\" {\n    height 2\n    chrome #false\n    border \"none\"\n    shell \"{{sh}}\"\n    command \"echo hi | tr i X\"\n}\n",
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(
        stdout.contains("hX"),
        "the shell ran the pipeline: {stdout}"
    );
    assert!(!stdout.contains("os error"), "{stdout}");
}

#[test]
fn an_inherited_shell_is_compared_after_expansion() {
    // `defaults { shell "fish" }` beside a pane's `shell="{{sh}}"` with
    // sh = "fish" is unequal AS WRITTEN and equal AS RUN. The
    // inherit-guards' own justification is the dialect the inherited
    // program was WRITTEN for, so resolved is the correct comparison —
    // and both guards (command and script) must take the same edit.
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, program_line) in [
        ("cmd.kdl", "command \"echo hi\""),
        ("script.kdl", "script \"echo hi\""),
    ] {
        let file = fixture(
            dir.path(),
            name,
            &format!(
                "variables {{\n    sh \"fish\"\n}}\n\ndefaults {{\n    height 3\n    shell \"fish\"\n    {program_line}\n}}\n\npane \"p\" shell=\"{{{{sh}}}}\" {{\n}}\n"
            ),
        );
        // A spawn of a missing `fish` is frame content, not an exit
        // code; a LOAD refusal is. The success assertion is the test.
        rat()
            .env("NO_COLOR", "1")
            .args(["dashboard", &file, "--once"])
            .assert()
            .success();
    }
}

// ─── rat dashboard check ────────────────────────────────────────────

/// A board whose command variable would append to `counter` if it ever
/// ran — the only honest way to prove a negative about a subprocess.
fn counting_board(counter: &std::path::Path, bin: &str) -> String {
    #[cfg(unix)]
    let counting = format!("printf x >> {}", counter.display());
    #[cfg(windows)]
    let counting = format!("echo x>> \"{}\"", counter.display());
    format!(
        "variables {{\n    store \"{counting}\" shell=#true\n}}\n\npane \"p\" {{\n    height 3\n    command \"{bin}\" \"style\" \"{{{{store}}}}\"\n}}\n",
        counting = counting.replace('\\', "\\\\").replace('"', "\\\""),
        bin = bin.replace('\\', "\\\\"),
    )
}

#[test]
fn a_valid_board_checks_clean() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            "variables {{\n    plan \"/tmp/x\"\n}}\n\npane \"p\" {{\n    height 3\n    command \"{bin}\" \"style\" \"{{{{plan}}}}\"\n}}\n",
            bin = rat_bin().replace('\\', "\\\\"),
        ),
    );
    let assert = rat()
        .args(["dashboard", "check", &file])
        .assert()
        .success()
        .stderr(predicates::str::is_empty());
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    // The presence anchor for the empty stderr: a clean board produced
    // a report, not nothing.
    assert!(stdout.contains("1 pane"), "{stdout}");
    assert!(stdout.contains("ok"), "{stdout}");
}

#[test]
fn check_never_runs_a_command_variable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let counter = dir.path().join("runs");
    let file = fixture(
        dir.path(),
        "board.kdl",
        &counting_board(&counter, &rat_bin()),
    );
    let assert = rat().args(["dashboard", "check", &file]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    // The presence anchor: the variable was SEEN, at its opaque tier —
    // exit 0 alone would also hold if check never read the block.
    assert!(stdout.contains("store"), "{stdout}");
    assert!(stdout.contains("opaque"), "{stdout}");
    assert!(!counter.exists(), "check ran the variable's command");
}

#[test]
fn check_never_runs_a_pane_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let counter = dir.path().join("runs");
    #[cfg(unix)]
    let counting = format!("printf x >> {}", counter.display());
    #[cfg(windows)]
    let counting = format!("echo x>> \"{}\"", counter.display());
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            "pane \"p\" {{\n    height 3\n    shell #true\n    command \"{counting}\"\n}}\n",
            counting = counting.replace('\\', "\\\\").replace('"', "\\\""),
        ),
    );
    let assert = rat().args(["dashboard", "check", &file]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    // The presence anchor: the pane was seen and deliberately not run.
    assert!(stdout.contains("1 pane"), "{stdout}");
    assert!(!counter.exists(), "check ran the pane's command");
}

#[test]
fn check_refuses_an_unknown_variable_with_its_place() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        "variables {\n    plan \"/tmp/x\"\n}\n\npane \"p\" {\n    height 3\n    command \"true\"\n    title \"{{plna}}\"\n}\n",
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", "check", &file])
        .assert()
        .failure()
        .stderr(predicates::str::contains("unknown variable `plna`"))
        .stderr(predicates::str::contains("line "))
        .stderr(predicates::str::contains("column "));
}

#[test]
fn check_reports_a_variable_cycle_as_a_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        "variables {\n    a \"{{b}}\"\n    b \"{{a}}\"\n}\n\npane \"p\" {\n    height 3\n    command \"true\"\n}\n",
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", "check", &file])
        .assert()
        .failure()
        .stderr(predicates::str::contains("variable cycle: `a` → `b` → `a`"));
}

#[test]
fn check_refuses_a_deferred_reference_at_a_load_time_site_by_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        "variables {\n    iv \"cd .\" shell=#true defer=#true\n}\n\npane \"p\" {\n    height 3\n    command \"true\"\n    interval \"{{iv}}\"\n}\n",
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", "check", &file])
        .assert()
        .failure()
        .stderr(predicates::str::contains("`iv`"))
        .stderr(predicates::str::contains("`interval`"));
}

#[test]
fn check_prints_exactly_what_a_run_would_print() {
    // The drift guard: the same failing board through both surfaces,
    // byte-identical stderr and the same exit code. The fixture's
    // failure must not depend on execution.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        "pane \"p\" {\n    height 3\n    command \"true\"\n    title \"{{nope}}\"\n}\n",
    );
    let run = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .failure();
    let check = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", "check", &file])
        .assert()
        .failure();
    assert_eq!(
        run.get_output().stderr,
        check.get_output().stderr,
        "the two surfaces drifted"
    );
    assert_eq!(
        run.get_output().status.code(),
        check.get_output().status.code()
    );
}

#[test]
fn a_malformed_constant_at_a_load_time_site_is_refused() {
    // F10 route 1, and the regression test for overrides-only
    // resolution: a constant needs no command to be known, and "bad"
    // is not a duration on any machine, ever.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        "variables {\n    iv \"bad\"\n}\n\npane \"p\" {\n    height 3\n    command \"true\"\n    interval \"{{iv}}\"\n}\n",
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", "check", &file])
        .assert()
        .failure()
        .stderr(predicates::str::contains("invalid duration"));
}

#[test]
fn a_command_derived_load_time_site_is_reported_as_skipped() {
    // F10 route 2: genuinely unknowable without running git, and
    // saying so is honest — exit 0, with the site AND the cause named.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            "variables {{\n    store \"git rev-parse\" shell=#true\n}}\n\npane \"events\" {{\n    height 3\n    command \"{bin}\" \"style\" \"x\"\n    trigger \"file:{{{{store}}}}/events\"\n}}\n",
            bin = rat_bin().replace('\\', "\\\\"),
        ),
    );
    let assert = rat().args(["dashboard", "check", &file]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(stdout.contains("trigger"), "{stdout}");
    assert!(stdout.contains("store"), "{stdout}");
    assert!(stdout.contains("not checked"), "{stdout}");
}

#[test]
fn a_constant_chain_is_checked_through_to_the_token() {
    // Known composes through chains rather than stopping at direct
    // constants: "5x" is not a duration.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        "variables {\n    a \"5\"\n    b \"{{a}}x\"\n}\n\npane \"p\" {\n    height 3\n    command \"true\"\n    interval \"{{b}}\"\n}\n",
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", "check", &file])
        .assert()
        .failure()
        .stderr(predicates::str::contains("invalid duration"));
}

#[test]
fn an_override_makes_a_once_at_load_variable_checkable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let board = format!(
        "variables {{\n    store \"git rev-parse\" shell=#true\n}}\n\npane \"events\" {{\n    height 3\n    command \"{bin}\" \"style\" \"x\"\n    trigger \"file:{{{{store}}}}/events\"\n}}\n",
        bin = rat_bin().replace('\\', "\\\\"),
    );
    let file = fixture(dir.path(), "board.kdl", &board);
    let assert = rat()
        .args(["dashboard", "check", &file, "-v", "store=/tmp/x"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(
        !stdout.contains("not checked"),
        "the override made it checkable: {stdout}"
    );
    assert!(stdout.contains("(-v)"), "the override is marked: {stdout}");
    // And a malformed override REFUSES — check legitimately checks
    // more with -v, never less.
    let sited = fixture(
        dir.path(),
        "sited.kdl",
        "variables {\n    iv \"cd .\" shell=#true\n}\n\npane \"p\" {\n    height 3\n    command \"true\"\n    interval \"{{iv}}\"\n}\n",
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", "check", &sited, "-v", "iv=nonsense"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("invalid duration"));
}

#[test]
fn an_override_does_not_rescue_a_deferred_variable_from_the_site_rule() {
    // The pair with the test above, differing in one word: defer.
    // -v makes the value Known but leaves the tier Spawn, and legality
    // is decided before expansion.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            "variables {{\n    store \"git rev-parse\" shell=#true defer=#true\n}}\n\npane \"events\" {{\n    height 3\n    command \"{bin}\" \"style\" \"x\"\n    trigger \"file:{{{{store}}}}/events\"\n}}\n",
            bin = rat_bin().replace('\\', "\\\\"),
        ),
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", "check", &file, "-v", "store=/tmp/x"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("`store`"))
        .stderr(predicates::str::contains("`trigger`"));
}

#[test]
fn a_raw_string_at_a_load_time_site_is_still_checked() {
    // A raw string's refs are empty by construction, so its bytes are
    // final — Expanded::Known — and a malformed one is CAUGHT. The one
    // place check is stronger than a reader expects.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        "pane \"p\" {\n    height 3\n    command \"true\"\n    trigger #\"not-a-scheme\"#\n}\n",
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", "check", &file])
        .assert()
        .failure()
        .stderr(predicates::str::contains("fifo:PATH, file:PATH, or fd:N"));
}

#[test]
fn a_skipped_site_names_the_command_not_the_constant() {
    // `sel` is on screen and is plainly a constant; `store` is the
    // command actually responsible. The chain answers the question the
    // reader is about to ask.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        &format!(
            "variables {{\n    store \"git rev-parse\" shell=#true\n    sel \"{{{{store}}}}/x\"\n}}\n\npane \"events\" {{\n    height 3\n    command \"{bin}\" \"style\" \"x\"\n    trigger \"file:{{{{sel}}}}\"\n}}\n",
            bin = rat_bin().replace('\\', "\\\\"),
        ),
    );
    let assert = rat().args(["dashboard", "check", &file]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(stdout.contains("store"), "names the command: {stdout}");
    assert!(
        stdout.contains("sel → store") || stdout.contains("sel \u{2192} store"),
        "shows the chain when the direct reference is a tainted constant: {stdout}"
    );
}

#[test]
fn every_load_time_site_is_reported() {
    // The consumer-side detector for check walking fewer sites than
    // load: an opaque variable at ALL TEN load-time string sites — the
    // nine derived from the pane schema plus the dashboard-level
    // title — must produce ten skipped rows. A site check never visits
    // is simply missing from the report, and no other test notices.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        "variables {\n    store \"git rev-parse\" shell=#true\n}\ntitle \"{{store}}\"\n\npane \"p\" {\n    height 7\n    command \"true\"\n    shell \"{{store}}\"\n    interval \"{{store}}\"\n    trigger-debounce \"{{store}}\"\n    trigger \"file:{{store}}\"\n    width \"{{store}}\"\n    overflow \"{{store}}\"\n    border \"{{store}}\"\n    padding \"{{store}}\"\n    title \"{{store}}\"\n}\n",
    );
    let assert = rat().args(["dashboard", "check", &file]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    for key in [
        "shell",
        "interval",
        "trigger-debounce",
        "trigger",
        "width",
        "overflow",
        "border",
        "padding",
    ] {
        assert!(stdout.contains(key), "{key} is reported: {stdout}");
    }
    assert!(
        stdout.contains("10 load-time sites were not checked"),
        "all ten sites, counted: {stdout}"
    );
    // The two titles are DIFFERENT sites sharing a spelling; the rows
    // must distinguish them.
    assert!(stdout.contains("pane \"p\" title"), "{stdout}");
    assert!(stdout.contains("the dashboard title"), "{stdout}");
}

#[test]
fn an_integer_key_holding_a_template_is_a_shape_error() {
    // height is outside the substitution surface by construction:
    // integers never pass the string chokepoint, so this is the
    // ordinary shape error, never a skipped site.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        "variables {\n    h \"7\"\n}\n\npane \"p\" {\n    height \"{{h}}\"\n    command \"true\"\n}\n",
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", "check", &file])
        .assert()
        .failure()
        .stderr(predicates::str::contains("one integer"));
}

#[test]
fn an_unused_variable_is_a_notice_not_a_refusal() {
    // A shipped template may declare a variable a consuming board is
    // expected to reference; refusing would make init's own output
    // fail check.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        "variables {\n    spare \"x\"\n}\n\npane \"p\" {\n    height 3\n    command \"true\"\n}\n",
    );
    let assert = rat().args(["dashboard", "check", &file]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(
        stdout.contains("spare") && stdout.contains("never referenced"),
        "{stdout}"
    );
}

#[test]
fn a_variable_supplied_on_the_command_line_checks_clean() {
    let dir = tempfile::tempdir().expect("tempdir");
    let counter = dir.path().join("runs");
    let file = fixture(
        dir.path(),
        "board.kdl",
        &counting_board(&counter, &rat_bin()),
    );
    let assert = rat()
        .args(["dashboard", "check", &file, "-v", "store=/given"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    // The presence anchor: the override was APPLIED, not merely the
    // file never parsed.
    assert!(stdout.contains("(-v)"), "{stdout}");
    assert!(!counter.exists(), "the suppressed command ran");
}

#[test]
fn check_writes_no_escapes_under_no_color() {
    use predicates::prelude::*;
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        "pane \"p\" {\n    height 3\n    command \"true\"\n    title \"{{nope}}\"\n}\n",
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", "check", &file])
        .assert()
        .failure()
        // The anchor: stderr is non-empty and carries the teaching
        // error — escape-free is trivially true of nothing.
        .stderr(predicates::str::contains("unknown variable `nope`"))
        .stderr(predicates::str::contains("\u{1b}").not());
}

#[test]
fn check_names_an_unreadable_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("missing.kdl");
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", "check", &missing.display().to_string()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("missing.kdl"));
}

#[test]
fn a_bare_dashboard_still_demands_a_file() {
    // What subcommand_negates_reqs buys, and the reason the flag is
    // there: exit 2, a usage error, unchanged.
    rat().args(["dashboard"]).assert().code(2);
}

#[test]
fn a_board_named_check_is_reachable_by_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "check",
        &format!(
            "pane \"p\" {{\n    height 2\n    chrome #false\n    border \"none\"\n    command \"{bin}\" \"style\" \"board-named-check\"\n}}\n",
            bin = rat_bin().replace('\\', "\\\\"),
        ),
    );
    let assert = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(stdout.contains("board-named-check"), "{stdout}");
}

// ─── rat dashboard init ─────────────────────────────────────────────

#[test]
fn init_writes_a_board_to_stdout() {
    // Bare `rat dashboard init` exits 1 TODAY (it reads `init` as a
    // filename), so the exit code alone is a vacuous red — the stdout
    // content is the assertion.
    let assert = rat().args(["dashboard", "init"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(stdout.contains("pane "), "{stdout}");
}

#[test]
fn the_written_board_is_one_rat_can_run() {
    // init and check prove each other.
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("board.kdl");
    let assert = rat().args(["dashboard", "init"]).assert().success();
    std::fs::write(&out, &assert.get_output().stdout).expect("write");
    rat()
        .args(["dashboard", "check", &out.display().to_string()])
        .assert()
        .success();
}

#[test]
fn list_names_every_template() {
    let assert = rat()
        .args(["dashboard", "init", "--list"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    for name in ["panes", "variables", "review"] {
        assert!(
            stdout.lines().any(|line| line.starts_with(name)),
            "{name} listed: {stdout}"
        );
    }
}

#[test]
fn an_unknown_template_lists_the_accepted_names() {
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", "init", "--template", "nope"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("nope"))
        .stderr(predicates::str::contains("panes"));
}

#[test]
fn output_writes_the_file_and_says_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("board.kdl");
    rat()
        .args(["dashboard", "init", "--output", &out.display().to_string()])
        .assert()
        .success()
        .stdout(predicates::str::is_empty());
    // The presence anchor for the empty stdout: the bytes landed in
    // the file instead.
    let written = std::fs::read_to_string(&out).expect("the file exists");
    assert!(written.contains("pane "), "{written}");
}

#[test]
fn output_refuses_to_overwrite_and_leaves_the_file_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("board.kdl");
    std::fs::write(&out, "keep-me").expect("seed");
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", "init", "--output", &out.display().to_string()])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("board.kdl"));
    // The byte assertion is the point, not the exit code.
    assert_eq!(
        std::fs::read_to_string(&out).expect("still there"),
        "keep-me"
    );
}

#[test]
fn the_review_template_declares_its_variables() {
    let assert = rat()
        .args(["dashboard", "init", "--template", "review"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(stdout.contains("variables {"), "{stdout}");
    assert!(stdout.contains("shell=#true"), "{stdout}");
    assert!(stdout.contains("defer=#true"), "{stdout}");
}

#[test]
fn the_review_template_names_the_handoff_file() {
    let assert = rat()
        .args(["dashboard", "init", "--template", "review"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(stdout.contains("handoff file"), "{stdout}");
}

#[test]
fn the_review_template_ships_no_syntax_this_release_cannot_parse() {
    // What keeps the commented action half commented.
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("review.kdl");
    let assert = rat()
        .args(["dashboard", "init", "--template", "review"])
        .assert()
        .success();
    std::fs::write(&out, &assert.get_output().stdout).expect("write");
    rat()
        .args(["dashboard", "check", &out.display().to_string()])
        .assert()
        .success();
}

#[test]
fn init_is_plain_text_under_every_profile() {
    // A declaration file is bytes for a file, never a frame — even a
    // FORCED color profile must not reach it. The presence anchor:
    // bytes were actually produced.
    for (key, value) in [("NO_COLOR", "1"), ("CLICOLOR_FORCE", "1")] {
        let assert = rat()
            .env(key, value)
            .args(["dashboard", "init"])
            .assert()
            .success();
        let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
        assert!(stdout.contains("pane "), "{stdout}");
        assert!(!stdout.contains('\u{1b}'), "{key}: {stdout}");
    }
}

#[test]
fn every_template_checks_clean_from_an_unrelated_directory() {
    // The LOAD gate, not a self-containment gate: check runs nothing,
    // and even a real run exits 0 with a missing inner board (a pane
    // failure renders in its own box). The static value-site scan is
    // the self-containment gate, and it lives beside the registry.
    let list = rat()
        .args(["dashboard", "init", "--list"])
        .assert()
        .success();
    let names: Vec<String> = String::from_utf8_lossy(&list.get_output().stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().next().map(str::to_string))
        .collect();
    assert!(!names.is_empty(), "the registry is non-empty");
    let dir = tempfile::tempdir().expect("a directory outside the repo");
    for name in names {
        let out = dir.path().join(format!("{name}.kdl"));
        rat()
            .args([
                "dashboard",
                "init",
                "--template",
                &name,
                "--output",
                &out.display().to_string(),
            ])
            .assert()
            .success();
        rat()
            .args(["dashboard", "check", &out.display().to_string()])
            .assert()
            .success();
    }
}

#[test]
fn the_variables_example_checks_clean() {
    rat()
        .args(["dashboard", "check", "examples/variables.kdl"])
        .assert()
        .success();
}

#[cfg(unix)]
#[test]
fn the_variables_example_renders_once() {
    // Parsing is not running: this is the route that proves the
    // expansions actually reach children. Unix, like the example's own
    // shell lines; the repo the test runs in is the git repository the
    // header asks for.
    let assert = rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", "examples/variables.kdl", "--once"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(stdout.contains("commits"), "{stdout}");
    assert!(
        stdout.contains("now at "),
        "the deferred value expanded: {stdout}"
    );
    assert!(
        stdout.contains("{{head}} means"),
        "the raw title kept its braces: {stdout}"
    );
}

#[test]
fn a_binding_without_a_description_names_itself_and_the_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        "key \"r\" {\n    command \"true\"\n}\n\npane \"a\" {\n    command \"true\"\n    height 3\n}\n",
    );
    rat()
        .env("NO_COLOR", "1")
        .args(["dashboard", &file, "--once"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "key \"r\": this binding needs a `description`",
        ))
        .stderr(predicates::str::contains("board.kdl"));
}

#[test]
fn a_binding_that_names_one_of_rats_own_keys_is_refused_at_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = fixture(
        dir.path(),
        "board.kdl",
        "key \"j\" {\n    description \"rerun\"\n    command \"true\"\n}\n\n\
         pane \"a\" {\n    command \"true\"\n    height 3\n}\n",
    );
    // Both board-validating routes: a refusal that fired on one and not
    // the other is the failure mode the shared prefix exists to close.
    for args in [
        vec!["dashboard", &file, "--once"],
        vec!["dashboard", "check", &file],
    ] {
        rat()
            .env("NO_COLOR", "1")
            .args(&args)
            .assert()
            .failure()
            .stderr(predicates::str::contains("`j` is one of rat's own keys"))
            .stderr(predicates::str::contains("scrolls down one line"))
            .stderr(predicates::str::contains("board.kdl"));
    }
}

/// The same board twice: without bindings, and with. Anything that
/// differs between these two beyond the `key` blocks makes the
/// comparison meaningless, so they are generated from one template —
/// and the helper asserts the only difference IS the key blocks.
fn board_pair(dir: &std::path::Path, keys: &str) -> (String, String) {
    let seeded = dir.join("seed.txt");
    std::fs::write(&seeded, "inert-needle\n").expect("seed");
    // `chrome #false`: the footer row carries a wall-clock stamp, and
    // two runs a second apart would differ in the clock rather than in
    // anything a binding could touch. The pane content and stderr are
    // the compared surface.
    let body = |keys: &str| {
        format!(
            "{keys}pane \"a\" {{\n    interval \"never\"\n    command \"{bin}\" \"__cat\" \"{seed}\"\n    height 3\n    chrome #false\n    border \"none\"\n}}\n",
            bin = rat_bin().escape_default(),
            seed = seeded.display().to_string().escape_default(),
        )
    };
    let (plain, bound) = (body(""), body(keys));
    assert_eq!(
        bound,
        format!("{keys}{plain}"),
        "the pair differs only in the key blocks"
    );
    (
        fixture(dir, "plain.kdl", &plain),
        fixture(dir, "bound.kdl", &bound),
    )
}

/// The `key` blocks the bound half of every pair declares. The command
/// is irrelevant on an inert route — nothing can press the key — but a
/// real program keeps the fixture honest.
const INERT_KEYS: &str =
    "key \"x\" {\n    description \"never reachable here\"\n    command \"true\"\n}\n\n";

/// Drain whatever else arrives inside the window, so a byte comparison
/// compares settled output rather than racing a late chunk.
fn settle(stream: &std::sync::mpsc::Receiver<Vec<u8>>, seen: &mut String) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(400);
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return;
        }
        match stream.recv_timeout(left) {
            Ok(chunk) => seen.push_str(&String::from_utf8_lossy(&chunk)),
            Err(_) => return,
        }
    }
}

/// One piped, live run of a board: spawn, read until the pane's
/// needle, settle, kill. Returns (stdout, stderr) as captured.
fn piped_live_capture(decl: &str) -> (String, String) {
    let dash = std::process::Command::new(rat_bin())
        .args(["dashboard", decl])
        .env("NO_COLOR", "1")
        .env("RAT_WIDTH", "60")
        .env("RAT_HEIGHT", "12")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn rat dashboard piped");
    let mut dash = KillOnDrop(dash);
    let out = stdout_stream(dash.0.stdout.take().expect("piped stdout"));
    let err = stderr_stream(dash.0.stderr.take().expect("piped stderr"));
    let mut seen_out = String::new();
    let mut seen_err = String::new();
    read_until(&out, &mut seen_out, "inert-needle");
    settle(&out, &mut seen_out);
    settle(&err, &mut seen_err);
    (seen_out, seen_err)
}

/// Inertness's byte-level half, piped live route. A witness rather than a
/// regression test: it was green the first time it ran, and its value
/// is that the binding transport can never move a piped board's bytes.
/// stderr matters as much as stdout — a binding diagnostic would land
/// there, and the invariant says no behavior change of ANY kind.
#[test]
fn a_piped_live_board_is_byte_identical_with_and_without_bindings() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plain, bound) = board_pair(dir.path(), INERT_KEYS);
    let (out_plain, err_plain) = piped_live_capture(&plain);
    let (out_bound, err_bound) = piped_live_capture(&bound);
    assert_eq!(out_plain, out_bound, "stdout moved");
    assert_eq!(err_plain, err_bound, "stderr moved");
}

/// Inertness's byte-level half, piped `--once` route — the cheapest
/// witness there is: two complete runs, compared byte for byte.
#[test]
fn a_piped_once_board_is_byte_identical_with_and_without_bindings() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (plain, bound) = board_pair(dir.path(), INERT_KEYS);
    let run = |decl: &str| {
        rat()
            .env("NO_COLOR", "1")
            .env("RAT_WIDTH", "60")
            .env("RAT_HEIGHT", "12")
            .args(["dashboard", decl, "--once"])
            .output()
            .expect("run rat dashboard --once")
    };
    let (a, b) = (run(&plain), run(&bound));
    assert!(a.status.success(), "plain board failed: {a:?}");
    assert!(b.status.success(), "bound board failed: {b:?}");
    assert_eq!(a.stdout, b.stdout, "stdout moved");
    assert_eq!(a.stderr, b.stderr, "stderr moved");
}

/// The anti-vacuity guard: two boards that both refused to load would
/// produce identical, empty output — green witnesses proving the
/// opposite of what they claim. This pins that the bound half loads
/// and renders, so the byte comparisons above compare something.
#[test]
fn a_board_declaring_bindings_still_loads_and_renders() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_, bound) = board_pair(dir.path(), INERT_KEYS);
    rat()
        .env("NO_COLOR", "1")
        .env("RAT_WIDTH", "60")
        .env("RAT_HEIGHT", "12")
        .args(["dashboard", &bound, "--once"])
        .assert()
        .success()
        .stdout(predicates::str::contains("inert-needle"));
}
