//! Smoke tests — verify the compiled binary starts and responds to basic flags.

use assert_cmd::Command;
use predicates::prelude::*;

fn agent() -> Command {
    Command::cargo_bin("agent").expect("binary should exist")
}

#[test]
fn version_flag() {
    agent()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("agent"));
}

#[test]
fn help_flag() {
    agent()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("AI"))
        .stdout(predicate::str::contains("--model"))
        .stdout(predicate::str::contains("--prompt"));
}

#[test]
fn dump_system_prompt() {
    agent()
        .arg("--dump-system-prompt")
        .assert()
        .success()
        .stdout(predicate::str::contains("tool"))
        .stdout(predicate::str::is_empty().not());
}

#[test]
fn unknown_flag_fails() {
    agent()
        .arg("--this-flag-does-not-exist")
        .assert()
        .failure()
        .stderr(predicate::str::contains("error").or(predicate::str::contains("unexpected")));
}

#[test]
fn prompt_mode_runs_and_exits() {
    // Verify one-shot mode produces output and exits cleanly.
    agent()
        .arg("--prompt")
        .arg("say ok")
        .arg("--max-turns")
        .arg("1")
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}
