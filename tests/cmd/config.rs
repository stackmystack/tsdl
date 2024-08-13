use assert_fs::prelude::*;
use indoc::formatdoc;
use predicates::{self as p};

use tsdl::{args::BuildCommand, consts::TSDL_BUILD_DIR};

use crate::cmd::Sandbox;

#[test]
fn no_args_shows_help() {
    let mut sandbox = Sandbox::new();
    sandbox
        .cmd
        .args(["config"])
        .assert()
        .failure()
        .stderr(p::str::starts_with("Configuration helpers"))
        .stderr(p::str::contains(format!(
            "Usage: {} config [OPTIONS] <COMMAND>",
            env!("CARGO_PKG_NAME")
        )));
    assert!(sandbox.is_empty());
}

#[test]
fn default_is_default_toml() {
    let mut sandbox = Sandbox::new();
    sandbox.cmd.args(["config", "default"]);
    sandbox.cmd.assert().success().stdout(p::str::contains(
        toml::to_string(&BuildCommand::default()).unwrap(),
    ));
    assert!(!sandbox.is_empty());
    sandbox
        .tmp
        .child(TSDL_BUILD_DIR)
        .child("log")
        .assert(p::path::exists())
        .assert(p::path::is_file());
}

#[test]
fn current_uses_default() {
    let mut sandbox = Sandbox::new();
    sandbox.cmd.args(["config", "current"]);
    sandbox
        .cmd
        .assert()
        .success()
        .stdout(p::str::contains(toml::to_string(&sandbox.build).unwrap()));
    assert!(!sandbox.is_empty());
    sandbox
        .tmp
        .child(TSDL_BUILD_DIR)
        .child("log")
        .assert(p::path::exists())
        .assert(p::path::is_file());
}

#[test]
fn current_uses_config_file() {
    let build_dir = "build-dir";
    let out_dir = "out-dir";
    let config = formatdoc! {
      r#"
        build-dir = "{build_dir}"
        out = "{out_dir}"
      "#
    };
    let mut sandbox = Sandbox::new();
    sandbox.config(&config);
    sandbox.cmd.args(["config", "current"]);
    sandbox
        .cmd
        .assert()
        .success()
        .stdout(p::str::contains(toml::to_string(&sandbox.build).unwrap()));
    assert!(!sandbox.is_empty());
    sandbox
        .tmp
        .child(build_dir)
        .child("log")
        .assert(p::path::exists())
        .assert(p::path::is_file());
}
