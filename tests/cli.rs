mod cmd;

use predicates::{self as p, prelude::PredicateBooleanExt};

use cmd::Sandbox;

#[test]
fn empty_dir_no_command_shows_help() {
    let mut sandbox = Sandbox::new();
    sandbox
        .cmd
        .assert()
        .failure()
        .stderr(
            p::str::contains(env!("CARGO_PKG_DESCRIPTION")).and(p::str::contains(format!(
                "Usage: {} [OPTIONS] <COMMAND>",
                env!("CARGO_PKG_NAME")
            ))),
        );
    assert!(sandbox.is_empty());
}
