use rstest::*;

use assert_fs::prelude::*;
use predicates::{self as p};
use tsdl::consts::BUILD_DIR;

use crate::cmd::Sandbox;

#[rstest]
fn build_no_args_should_log_to_default_path() {
    let mut sandbox = Sandbox::new();
    sandbox.cmd.arg("build");
    sandbox
        .cmd
        .assert()
        .success()
        .stdout(p::str::contains("tree-sitter-cli  [2/2] done"));
    assert!(!sandbox.is_empty());
    sandbox
        .tmp
        .child(BUILD_DIR)
        .child("log")
        .assert(p::path::exists())
        .assert(p::path::is_file());
}

#[rstest]
#[case::cwd("tsdl.log")]
#[case::child_dir("here/log")]
#[case::build_dir_root("tmp/custom.log")]
#[case::absolute("/tmp/tsdl.log")]
#[case::parent("../tsdl.log")]
fn build_w_specific_log_path(#[case] log: &str) {
    let mut sandbox = Sandbox::new();
    sandbox.cmd.args(["build", "--log", log]);
    sandbox
        .cmd
        .assert()
        .success()
        .stdout(p::str::contains("tree-sitter-cli  [2/2] done"));
    sandbox
        .tmp
        .child(log)
        .assert(p::path::exists())
        .assert(p::path::is_file());
}

#[rstest]
#[case::nested_in_build_dir("tmp/logs/tsdl.log", "nested inside --build-dir")]
#[case::lock_file("tmp/tsdl.lock", "conflicts with a tsdl runtime/build file")]
#[case::cache_file("tmp/cache.toml", "conflicts with a tsdl runtime/build file")]
fn build_rejects_log_paths_that_conflict_with_fresh_cleanup(
    #[case] log: &str,
    #[case] expected: &str,
) {
    let mut sandbox = Sandbox::new();
    sandbox.cmd.args(["build", "--log", log]);
    sandbox
        .cmd
        .assert()
        .failure()
        .stderr(p::str::contains(expected));
}

#[rstest]
fn fresh_preserves_root_level_custom_log_and_removes_build_entries() {
    let mut sandbox = Sandbox::new();
    let stale = sandbox.tmp.child(BUILD_DIR).child("stale.txt");
    stale.write_str("stale").unwrap();

    sandbox
        .cmd
        .args(["build", "--fresh", "--log", "tmp/custom.log"]);
    sandbox.cmd.assert().success();

    sandbox
        .tmp
        .child(BUILD_DIR)
        .child("custom.log")
        .assert(p::path::exists())
        .assert(p::path::is_file());
    stale.assert(p::path::missing());
}
