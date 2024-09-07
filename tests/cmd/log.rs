use rstest::*;

use assert_fs::prelude::*;
use predicates::{self as p};
use tsdl::consts::{TREE_SITTER_VERSION, TSDL_BUILD_DIR};

use crate::cmd::Sandbox;

#[rstest]
fn build_no_args_should_log_to_default_path() {
    let mut sandbox = Sandbox::new();
    sandbox.cmd.arg("build");
    sandbox
        .cmd
        .assert()
        .success()
        .stderr(p::str::contains(format!(
            "tree-sitter-cli v{TREE_SITTER_VERSION} done"
        )));
    assert!(!sandbox.is_empty());
    sandbox
        .tmp
        .child(TSDL_BUILD_DIR)
        .child("log")
        .assert(p::path::exists())
        .assert(p::path::is_file());
}

#[rstest]
#[case::cwd("tsdl.log")]
#[case::child_dir("here/log")]
#[case::absolute("/tmp/tsdl.log")]
#[case::parent("../tsdl.log")]
fn build_w_specific_log_path(#[case] log: &str) {
    let mut sandbox = Sandbox::new();
    sandbox.cmd.args(["build", "--log", log]);
    sandbox
        .cmd
        .assert()
        .success()
        .stderr(p::str::contains(format!(
            "tree-sitter-cli v{TREE_SITTER_VERSION} done"
        )));
    sandbox
        .tmp
        .child(log)
        .assert(p::path::exists())
        .assert(p::path::is_file());
}
