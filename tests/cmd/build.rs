use std::{env::consts::DLL_EXTENSION, os::unix::fs::PermissionsExt};

use assert_cmd::Command;
use assert_fs::prelude::*;
use indoc::{formatdoc, indoc};
use predicates::{self as p};
use rstest::*;

use tsdl::consts::{
    TREE_SITTER_PLATFORM, TREE_SITTER_VERSION, TSDL_BUILD_DIR, TSDL_CONFIG_FILE, TSDL_OUT_DIR,
    TSDL_PREFIX,
};

#[cfg(enable_wasm_cases)]
use tsdl::parser::WASM_EXTENSION;

use crate::cmd::Sandbox;

#[rstest]
fn no_args_should_download_tree_sitter_cli() {
    let mut sandbox = Sandbox::new();
    sandbox.cmd.arg("build");
    sandbox
        .cmd
        .assert()
        .success()
        .stdout(p::str::contains(format!(
            "tree-sitter-cli v{TREE_SITTER_VERSION}"
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

    let gz = tree_sitter_cli.with_extension("gz");
    assert!(!gz.exists());
}

#[rstest]
#[case::no_leading_v("0.25.6", "v0.25.6", "0.25.6")]
#[case::leading_v("v0.25.6", "v0.25.6", "0.25.6")]
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
        .stdout(p::str::contains(format!("tree-sitter-cli {version}")));
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
        assert = assert.stdout(p::str::contains(format!("{lang} HEAD cloning")));
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
fn test_real_parser_error_formatting() {
    let mut sandbox = Sandbox::new();
    let output = sandbox.cmd.arg("build").args(["jsonxxx"]).output().unwrap();

    // Should fail
    assert!(!output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);

    // Extract just the error part (after the progress messages)
    let error_part = stderr
        .lines()
        .skip_while(|line| !line.contains("Could not build all parsers"))
        .collect::<Vec<_>>()
        .join("\n");

    // MacOS needs the canonicalize because tmp by default doesn't have /private as root.
    let build_dir = std::fs::canonicalize(
        sandbox
            .tmp
            .path()
            .join(TSDL_BUILD_DIR)
            .join("tree-sitter-jsonxxx"),
    )
    .unwrap();

    // Define the exact expected error format using multi-line string literal
    let expected = format!(
        "\
Could not build all parsers.

  jsonxxx: Could not clone to {}.
      $ git fetch origin --depth 1 HEAD failed with exit status 128.
      fatal: could not read Username for 'https://github.com': terminal prompts disabled\
",
        build_dir.display()
    );

    // Cursor for sequential searching because some shells might output noise.
    let mut remaining_output = error_part.as_str();

    for line in expected.lines() {
        if line.trim().is_empty() {
            continue;
        }

        // Find exact line (w/ indentation) within the remaining slice
        if let Some(idx) = remaining_output.find(line) {
            // Move cursor past the found line to ensure order
            remaining_output = &remaining_output[idx + line.len()..];
        } else {
            panic!(
                "Output mismatch.\n\
                 Could not find expected line (or it is out of order):\n\
                 {line:?}\n\
                 \n\
                 Inside remaining output:\n\
                 {remaining_output:?}\n\
                 \n\
                 Original full output:\n\
                 {error_part}"
            );
        }
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
        assert = assert.stdout(p::str::contains(format!("{lang} HEAD cloning")));
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
        .stdout(p::str::contains(format!(
            "{language}/{language} {version} build done"
        )));
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
        out = out.stdout(p::str::contains(format!(
            "{language}/{language} {version} build done"
        )));
    }
    for (language, _version) in parsers {
        let dylib = sandbox
            .tmp
            .child(TSDL_OUT_DIR)
            .child(format!("{TSDL_PREFIX}{language}.{DLL_EXTENSION}"));
        dylib.assert(p::path::exists()).assert(p::path::is_file());
    }
}

#[rstest]
fn multi_parsers_no_cmd() {
    let java = "java";
    let version = "HEAD";
    let languages = [java];
    let mut sandbox = Sandbox::new();
    let mut assert = sandbox.cmd.args(["build", java]).assert().success();
    for language in languages {
        assert = assert.stdout(p::str::contains(format!("{language} {version} cloning")));
    }
    for language in languages {
        let dylib = sandbox
            .tmp
            .child(TSDL_OUT_DIR)
            .child(format!("{TSDL_PREFIX}{language}.{DLL_EXTENSION}"));
        dylib.assert(p::path::exists()).assert(p::path::is_file());
    }
}

#[rstest]
fn multi_parsers_cmd() {
    let typescript = "typescript";
    let version = "0.21.0";
    let languages = [typescript, "tsx"];
    let mut sandbox = Sandbox::new();
    let config = formatdoc! {
      r#"
        [parsers]
        typescript = {{ ref = "{version}", cmd = "make" }}
      "#
    };
    sandbox
        .tmp
        .child(TSDL_CONFIG_FILE)
        .write_str(&config)
        .unwrap();
    let assert = sandbox.cmd.args(["build", typescript]).assert().success();
    // Check for version in cloning step
    // TODO: dig for changes in this test and revert.
    _ = assert.stdout(p::str::contains(format!("{typescript} v{version} cloning")));
    for language in languages {
        let dylib = sandbox
            .tmp
            .child(TSDL_OUT_DIR)
            .child(format!("{TSDL_PREFIX}{language}.{DLL_EXTENSION}"));
        dylib.assert(p::path::exists()).assert(p::path::is_file());
    }
}

#[rstest]
#[case::default(None, &[DLL_EXTENSION])]
#[cfg_attr(enable_wasm_cases, case::all(Some("all"), &[DLL_EXTENSION, WASM_EXTENSION]))]
#[case::native(Some("native"), &[DLL_EXTENSION])]
#[cfg_attr(enable_wasm_cases, case::wasm(Some("wasm"), &[WASM_EXTENSION]))]
fn build_target(#[case] target: Option<&str>, #[case] exts: &[&str]) {
    use std::fmt::Write as _;

    let languages = [("json", "0.21.0")];
    let mut config = String::new();
    writeln!(config, "[parsers]").unwrap();
    for (lang, ver) in languages {
        writeln!(config, "  {lang} = \"{ver}\"").unwrap();
    }
    if let Some(target) = target {
        config = format!("target = \"{target}\"\n{config}");
    }
    let mut sandbox = Sandbox::new();
    sandbox
        .tmp
        .child(TSDL_CONFIG_FILE)
        .write_str(&config)
        .unwrap();
    sandbox.cmd.args(["build"]).assert().success();
    for (lang, _) in languages {
        for ext in exts {
            let dylib = sandbox
                .tmp
                .child(TSDL_OUT_DIR)
                .child(format!("{TSDL_PREFIX}{lang}.{ext}"));
            dylib.assert(p::path::exists()).assert(p::path::is_file());
        }
    }
}

#[rstest]
fn build_plain_progress_numbered_correctly() {
    let mut sandbox = Sandbox::new();
    let output = sandbox
        .cmd
        .args(["build", "json", "--progress=plain"])
        .output()
        .unwrap();

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verify that steps are numbered starting from 1, not 0
    assert!(stdout.contains("[1/"), "stdout should contain [1/");
    assert!(stdout.contains("[2/"), "stdout should contain [2/");
    assert!(stdout.contains("[3/"), "stdout should contain [3/");

    // Verify no [0/ appears (which was the bug)
    assert!(
        !stdout.contains("[0/"),
        "stdout should not contain [0/ (step numbering started at 0)"
    );

    // Verify the output artifact was created
    let dylib = sandbox
        .tmp
        .child(TSDL_OUT_DIR)
        .child(format!("{TSDL_PREFIX}json.{DLL_EXTENSION}"));
    dylib.assert(p::path::exists()).assert(p::path::is_file());
}
