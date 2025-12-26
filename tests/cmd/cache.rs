use std::{env::consts::DLL_EXTENSION, os::unix::fs::MetadataExt};

use assert_cmd::cargo::cargo_bin_cmd;
use assert_fs::prelude::*;
use predicates::{self as p, prelude::*};
use rstest::*;

use tsdl::consts::{TSDL_BUILD_DIR, TSDL_OUT_DIR, TSDL_PREFIX};

use crate::cmd::Sandbox;

#[rstest]
fn cache_hit_skips_build() {
    let mut sandbox = Sandbox::new();

    // First build
    sandbox.cmd.arg("build").arg("json").assert().success();

    let binary = sandbox
        .tmp
        .child(TSDL_OUT_DIR)
        .child(format!("{TSDL_PREFIX}json.{DLL_EXTENSION}"));
    binary.assert(p::path::exists()).assert(p::path::is_file());

    // Cache file should exist
    let cache_file = sandbox.tmp.child(TSDL_BUILD_DIR).child("cache.toml");
    cache_file.assert(p::path::exists());

    let first_inode = binary.metadata().unwrap().ino();

    // Second build in same sandbox should hit cache
    let mut cmd = cargo_bin_cmd!();
    cmd.current_dir(sandbox.tmp.path());
    cmd.arg("build")
        .arg("json")
        .assert()
        .success()
        .stderr(p::str::contains("json HEAD (cached)"))
        .stderr(p::str::contains("Cloning").not());

    let second_inode = binary.metadata().unwrap().ino();
    assert_eq!(
        first_inode, second_inode,
        "Inode should remain the same on cache hit"
    );
}

#[rstest]
fn cache_miss_on_grammar_modification() {
    let mut sandbox = Sandbox::new();

    // First build
    sandbox.cmd.arg("build").arg("json").assert().success();

    // Modify grammar file
    let grammar = sandbox
        .tmp
        .child(TSDL_BUILD_DIR)
        .child("tree-sitter-json")
        .child("grammar.js");
    let mut content = std::fs::read_to_string(grammar.path()).unwrap();
    content.push_str("\n// test modification\n");
    grammar.write_str(&content).unwrap();

    // Second build should miss cache (use --force because binary already exists with different inode)
    let mut cmd = cargo_bin_cmd!();
    cmd.current_dir(sandbox.tmp.path());
    cmd.args(["build", "--force", "json"])
        .assert()
        .success()
        .stderr(p::str::contains("json Cloning"))
        .stderr(p::str::contains("(cached)").not());
}

#[rstest]
fn fresh_flag_clears_build_dir() {
    let mut sandbox = Sandbox::new();

    // First build
    sandbox.cmd.arg("build").arg("json").assert().success();

    let build_dir = sandbox.tmp.child(TSDL_BUILD_DIR);
    build_dir.assert(p::path::exists());

    let cache_file = build_dir.child("cache.toml");
    cache_file.assert(p::path::exists());

    let first_binary = sandbox
        .tmp
        .child(TSDL_OUT_DIR)
        .child(format!("{TSDL_PREFIX}json.{DLL_EXTENSION}"));
    let first_inode = first_binary.metadata().unwrap().ino();

    // Second build with --fresh (need --force to overwrite existing binary)
    let mut cmd = cargo_bin_cmd!();
    cmd.current_dir(sandbox.tmp.path());
    cmd.args(["build", "--fresh", "--force", "json"])
        .assert()
        .success();

    // Cache file should be gone and recreated
    cache_file.assert(p::path::exists());

    let second_inode = first_binary.metadata().unwrap().ino();

    assert_ne!(
        first_inode, second_inode,
        "Fresh build should create new binary with different inode"
    );
}

#[rstest]
fn force_flag_bypasses_cache() {
    let mut sandbox = Sandbox::new();

    // First build
    sandbox.cmd.arg("build").arg("json").assert().success();

    let binary = sandbox
        .tmp
        .child(TSDL_OUT_DIR)
        .child(format!("{TSDL_PREFIX}json.{DLL_EXTENSION}"));
    let first_inode = binary.metadata().unwrap().ino();

    // Second build with --force
    let mut cmd = cargo_bin_cmd!();
    cmd.current_dir(sandbox.tmp.path());
    cmd.args(["build", "--force", "json"])
        .assert()
        .success()
        .stderr(p::str::contains("json Cloning"))
        .stderr(p::str::contains("(cached)").not());

    let second_inode = binary.metadata().unwrap().ino();
    assert_ne!(
        first_inode, second_inode,
        "--force should create new binary with different inode"
    );
}

#[rstest]
fn force_flag_reinstalls_hardlink() {
    let mut sandbox = Sandbox::new();

    // First build
    sandbox.cmd.arg("build").arg("json").assert().success();

    let binary = sandbox
        .tmp
        .child(TSDL_OUT_DIR)
        .child(format!("{TSDL_PREFIX}json.{DLL_EXTENSION}"));
    let build_binary = sandbox
        .tmp
        .child(TSDL_BUILD_DIR)
        .child("tree-sitter-json")
        .child(format!("libtree-sitter-json.{DLL_EXTENSION}"));

    let first_inode_out = binary.metadata().unwrap().ino();
    let first_inode_build = build_binary.metadata().unwrap().ino();
    assert_eq!(
        first_inode_out, first_inode_build,
        "Hard-link should have same inode"
    );

    // Replace output binary with a copy (different inode)
    let content = std::fs::read(binary.path()).unwrap();
    std::fs::remove_file(binary.path()).unwrap();
    std::fs::write(binary.path(), &content).unwrap();

    let broken_inode_out = binary.metadata().unwrap().ino();
    assert_ne!(
        broken_inode_out, first_inode_build,
        "Replaced binary should have different inode"
    );

    // Second build with --force should fix the hard-link
    let mut cmd = cargo_bin_cmd!();
    cmd.current_dir(sandbox.tmp.path());
    cmd.args(["build", "--force", "json"])
        .assert()
        .success()
        .stderr(p::str::contains("--force"));

    let final_inode_out = binary.metadata().unwrap().ino();
    let final_inode_build = build_binary.metadata().unwrap().ino();
    assert_eq!(
        final_inode_out, final_inode_build,
        "After --force, hard-link should be restored"
    );
}

#[rstest]
#[case::json_and_python(vec!["json", "python"])]
fn multi_parser_independent_cache(#[case] languages: Vec<&str>) {
    let mut sandbox = Sandbox::new();

    // First build all parsers
    sandbox.cmd.arg("build").args(&languages).assert().success();

    // Verify cache contains both entries
    let cache_file = sandbox.tmp.child(TSDL_BUILD_DIR).child("cache.toml");
    let cache_content = std::fs::read_to_string(cache_file.path()).unwrap();
    for lang in &languages {
        assert!(
            cache_content.contains(&format!("[parsers.{lang}]")),
            "Cache should contain entry for {lang}",
        );
    }

    // Second build without modification should hit cache for both
    let mut cmd = cargo_bin_cmd!();
    cmd.current_dir(sandbox.tmp.path());
    let mut output = cmd.arg("build").args(&languages).assert().success();

    for lang in &languages {
        output = output.stderr(p::str::contains(format!("{lang} HEAD (cached)")));
    }
}

#[rstest]
fn cache_file_structure() {
    let mut sandbox = Sandbox::new();

    // Build two parsers
    sandbox
        .cmd
        .arg("build")
        .args(["json", "python"])
        .assert()
        .success();

    // Read and validate cache file
    let cache_file = sandbox.tmp.child(TSDL_BUILD_DIR).child("cache.toml");
    cache_file.assert(p::path::exists());

    let cache_content = std::fs::read_to_string(cache_file.path()).unwrap();

    // Verify TOML structure contains expected entries
    assert!(
        cache_content.contains("[parsers.json]"),
        "Cache should have json entry"
    );
    assert!(
        cache_content.contains("[parsers.python]"),
        "Cache should have python entry"
    );
    assert!(
        cache_content.contains("grammar_sha1"),
        "Cache should have grammar_sha1 field"
    );
    assert!(
        cache_content.contains("git_ref"),
        "Cache should have git_ref field"
    );
    assert!(
        cache_content.contains("timestamp"),
        "Cache should have timestamp field"
    );
}
