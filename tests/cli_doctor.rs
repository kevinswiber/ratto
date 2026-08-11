mod common;

use common::rat;

#[test]
fn json_reports_the_default_appearance_under_piped_stdio() {
    // assert_cmd pipes all three stdio streams, so stderr is not a terminal
    // and the query is never emitted. No verdict reaches the palette, which
    // is what "default" records.
    rat()
        .args(["doctor", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"appearance\":\"dark\""))
        .stdout(predicates::str::contains(
            "\"appearance_source\":\"default\"",
        ));
}

#[test]
fn an_explicit_appearance_is_reported_as_explicit() {
    rat()
        .args(["--appearance", "light", "doctor", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"appearance\":\"light\""))
        .stdout(predicates::str::contains(
            "\"appearance_source\":\"explicit\"",
        ));
}

#[test]
fn a_requested_appearance_reports_the_same_way_however_it_arrived() {
    // The flag and the environment variable are one argument, so both
    // report `explicit` and cannot be distinguished.
    rat()
        .env("RAT_APPEARANCE", "dark")
        .args(["doctor", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"appearance\":\"dark\""))
        .stdout(predicates::str::contains(
            "\"appearance_source\":\"explicit\"",
        ));
}

#[test]
fn the_text_report_names_the_appearance_and_its_source() {
    rat()
        .args(["--appearance", "light", "doctor"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Appearance:"))
        .stdout(predicates::str::contains("light (explicit)"));
}

#[test]
fn a_session_is_not_accessible_by_default() {
    rat()
        .args(["doctor", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"accessible\":false"))
        .stdout(predicates::str::contains("\"accessible_reason\":\"unset\""));
}

#[test]
fn an_accessible_session_names_the_command_line() {
    rat()
        .args(["--accessible", "doctor", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"accessible\":true"))
        .stdout(predicates::str::contains(
            "\"accessible_reason\":\"command line\"",
        ));
}

#[test]
fn the_variable_is_named_as_the_reason_when_it_is_set() {
    rat()
        .env("RAT_ACCESSIBLE", "1")
        .args(["doctor", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"accessible\":true"))
        .stdout(predicates::str::contains(
            "\"accessible_reason\":\"RAT_ACCESSIBLE=1\"",
        ));
}

#[test]
fn an_overridden_variable_is_reported_with_both_halves() {
    // The one input where reading the environment and reading the
    // resolved state DISAGREE: the variable asked for the transcript and
    // something on the command line beat it. A report that derived the
    // state from the variable would say `true` here and pass every other
    // test in this file.
    rat()
        .env("RAT_ACCESSIBLE", "1")
        .args(["--no-accessible", "doctor", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"accessible\":false"))
        .stdout(predicates::str::contains(
            "\"accessible_reason\":\"RAT_ACCESSIBLE=1\"",
        ));
}

#[test]
fn the_text_report_names_the_accessible_state_and_its_source() {
    rat()
        .args(["--accessible", "doctor"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "Accessible:       on (command line)",
        ));
}
