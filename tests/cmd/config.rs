use assert_fs::prelude::*;
use indoc::formatdoc;
use predicates::{self as p};

use tsdl::{
  args::BuildCommand,
  consts::{BUILD_DIR, CONFIG_FILE},
};

use crate::cmd::Sandbox;

#[test]
fn current_rejects_malformed_config_file() {
  let mut sandbox = Sandbox::new();
  sandbox
    .tmp
    .child(CONFIG_FILE)
    .write_str("not valid toml =")
    .unwrap();

  sandbox
    .cmd
    .args(["config", "current"])
    .assert()
    .failure()
    .stderr(p::str::contains(
      "Resolving build configuration for `config current`",
    ))
    .stderr(p::str::contains("Parsing config file"));
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
  sandbox.tmp.child(build_dir).assert(p::path::missing());
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
  assert!(sandbox.is_empty());
}

#[test]
fn current_uses_explicit_log_file() {
  let mut sandbox = Sandbox::new();
  sandbox
    .cmd
    .args(["--log", "config-current.log", "config", "current"]);
  sandbox.cmd.assert().success();
  sandbox
    .tmp
    .child("config-current.log")
    .assert(p::path::exists())
    .assert(p::path::is_file());
  sandbox.tmp.child(BUILD_DIR).assert(p::path::missing());
}

#[test]
fn default_ignores_malformed_config_file() {
  let mut sandbox = Sandbox::new();
  sandbox
    .tmp
    .child(CONFIG_FILE)
    .write_str("not valid toml =")
    .unwrap();

  sandbox
    .cmd
    .args(["config", "default"])
    .assert()
    .success()
    .stdout(p::str::contains(
      toml::to_string(&BuildCommand::default()).unwrap(),
    ));
}

#[test]
fn default_is_default_toml() {
  let mut sandbox = Sandbox::new();
  sandbox.cmd.args(["config", "default"]);
  sandbox.cmd.assert().success().stdout(p::str::contains(
    toml::to_string(&BuildCommand::default()).unwrap(),
  ));
  assert!(sandbox.is_empty());
}

#[test]
fn default_uses_explicit_log_file() {
  let mut sandbox = Sandbox::new();
  sandbox
    .cmd
    .args(["--log", "config-default.log", "config", "default"]);
  sandbox.cmd.assert().success();
  sandbox
    .tmp
    .child("config-default.log")
    .assert(p::path::exists())
    .assert(p::path::is_file());
  sandbox.tmp.child(BUILD_DIR).assert(p::path::missing());
}

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
