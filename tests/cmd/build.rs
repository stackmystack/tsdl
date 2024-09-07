use std::{env::consts::DLL_EXTENSION, os::unix::fs::PermissionsExt};

use assert_cmd::Command;
use assert_fs::prelude::*;
use indoc::indoc;
use predicates::{self as p};
use rstest::*;

use tsdl::consts::{
    TREE_SITTER_PLATFORM, TREE_SITTER_VERSION, TSDL_BUILD_DIR, TSDL_CONFIG_FILE, TSDL_OUT_DIR,
    TSDL_PREFIX,
};

use crate::cmd::Sandbox;

#[rstest]
fn no_args_should_download_tree_sitter_cli() {
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
    let tree_sitter_cli = sandbox
        .tmp
        .child(TSDL_BUILD_DIR)
        .child(format!("tree-sitter-{TREE_SITTER_PLATFORM}"));

    tree_sitter_cli
        .assert(p::path::exists())
        .assert(p::path::is_file());

    let tree_sitter_cli = tree_sitter_cli.to_path_buf();
    assert!(tree_sitter_cli.metadata().unwrap().permissions().mode() & 0o111 != 0);
}

#[rstest]
#[case::no_leading_v("0.22.0", "v0.22.0", "0.22.0")]
#[case::leading_v("v0.22.0", "v0.22.0", "0.22.0")]
#[case::sha1("636801770eea172d140e64b691815ff11f6b556f", "6368017", "0.22.6")]
fn no_args_should_build_tree_sitter_with_specific_version(
    #[case] requested: &str,
    #[case] version: &str,
    #[case] cli_version: &str,
) {
    let mut sandbox = Sandbox::new();
    sandbox
        .cmd
        .args(["build", "--tree-sitter-version", requested]);
    sandbox
        .cmd
        .assert()
        .success()
        .stderr(p::str::contains(format!("tree-sitter-cli {version} done")));
    let mut tree_sitter_cli = Command::new(
        sandbox
            .tmp
            .child(TSDL_BUILD_DIR)
            .child(format!("tree-sitter-{TREE_SITTER_PLATFORM}"))
            .to_path_buf(),
    );
    tree_sitter_cli.arg("--version");
    tree_sitter_cli
        .assert()
        .success()
        .stdout(p::str::contains(format!("tree-sitter {cli_version}")));
}

#[rstest]
#[case::gringo(vec!["gringo"])]
#[case::gringo_bringo(vec!["gringo", "bringo"])]
fn unknown_parser_should_fail(#[case] languages: Vec<&str>) {
    let mut sandbox = Sandbox::new();
    sandbox.cmd.arg("build").args(&languages);
    let mut assert = sandbox.cmd.assert().failure();
    for lang in &languages {
        assert = assert.stderr(p::str::contains(format!("{lang} HEAD failed")));
    }
    for lang in languages {
        sandbox
            .tmp
            .child(TSDL_OUT_DIR)
            .child(format!("{lang}.{DLL_EXTENSION}"))
            .assert(p::path::missing());
    }
}

#[rstest]
#[case::json(vec!["json"])]
#[case::json_rust(vec!["json", "rust"])]
fn no_config_should_build_valid_parser_from_head(#[case] languages: Vec<&str>) {
    let mut sandbox = Sandbox::new();
    sandbox.cmd.arg("build").args(&languages);
    let mut assert = sandbox.cmd.assert().success();
    for lang in &languages {
        assert = assert.stderr(p::str::contains(format!("{lang} HEAD done")));
    }
    for lang in &languages {
        let dylib = sandbox
            .tmp
            .child(TSDL_OUT_DIR)
            .child(format!("{TSDL_PREFIX}{lang}.{DLL_EXTENSION}"));
        dylib.assert(p::path::exists()).assert(p::path::is_file());
    }
}

#[rstest]
#[case::pinned_hash_and_from_cobol("cobol", "6a46906")]
#[case::pinned_leading_v_java("java", "v0.21.0")]
#[case::pinned_master_python("python", "master")]
#[case::pinned_no_leading_v_json("json", "v0.21.0")]
#[case::unpinned_rust("rust", "HEAD")]
#[case::pinned::cmd::typescript("typescript", "v0.21.0")]
fn build_explicit_pinned_and_unpinned(#[case] language: &str, #[case] version: &str) {
    let config = indoc! {
      r#"
        [parsers]
        java = "v0.21.0"
        json = "0.21.0"
        python = "master"
        typescript = { ref = "0.21.0", cmd = "make" }
        cobol = { ref = "6a469068cacb5e3955bb16ad8dfff0dd792883c9", from = "https://github.com/yutaro-sakamoto/tree-sitter-cobol" }
      "#
    };
    let mut sandbox = Sandbox::new();
    sandbox
        .tmp
        .child(TSDL_CONFIG_FILE)
        .write_str(config)
        .unwrap();
    sandbox
        .cmd
        .args(["build", language])
        .assert()
        .success()
        .stderr(p::str::contains(format!("{language} {version} done")));
    let dylib = sandbox
        .tmp
        .child(TSDL_OUT_DIR)
        .child(format!("{TSDL_PREFIX}{language}.{DLL_EXTENSION}"));
    dylib.assert(p::path::exists()).assert(p::path::is_file());
}

#[rstest]
fn build_implicit_pinned_and_unpinned() {
    let parsers = [
        ("cobol", "6a46906"),
        ("java", "v0.21.0"),
        ("python", "master"),
        ("json", "v0.21.0"),
        ("typescript", "v0.21.0"),
    ];
    let config = indoc! {
      r#"
        [parsers]
        java = "v0.21.0"
        json = "0.21.0"
        python = "master"
        typescript = { ref = "0.21.0", cmd = "make" }
        cobol = { ref = "6a469068cacb5e3955bb16ad8dfff0dd792883c9", from = "https://github.com/yutaro-sakamoto/tree-sitter-cobol" }
      "#
    };
    let mut sandbox = Sandbox::new();
    sandbox
        .tmp
        .child(TSDL_CONFIG_FILE)
        .write_str(config)
        .unwrap();
    let mut out = sandbox.cmd.arg("build").assert().success();
    for (language, version) in parsers {
        out = out.stderr(p::str::contains(format!("{language} {version} done")));
    }
    for (language, _version) in parsers {
        let dylib = sandbox
            .tmp
            .child(TSDL_OUT_DIR)
            .child(format!("{TSDL_PREFIX}{language}.{DLL_EXTENSION}"));
        dylib.assert(p::path::exists()).assert(p::path::is_file());
    }
}

#[test]
fn multi_parsers_no_cmd() {
    let php = "php";
    let version = "HEAD";
    let languages = ["php", "php_only"];
    let mut sandbox = Sandbox::new();
    let mut assert = sandbox.cmd.args(["build", "php"]).assert().success();
    for language in languages {
        assert = assert.stderr(p::str::contains(format!(
            "{php}: Building {version} parser: {language}"
        )));
    }
    for language in languages {
        let dylib = sandbox
            .tmp
            .child(TSDL_OUT_DIR)
            .child(format!("{TSDL_PREFIX}{language}.{DLL_EXTENSION}"));
        dylib.assert(p::path::exists()).assert(p::path::is_file());
    }
}

// TODO:
// #[case::pinned::cmd::typescript("typescript", "v0.21.0")]
// multi_parsers_cmd
