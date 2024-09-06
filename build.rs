use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use cargo_metadata::MetadataCommand;
use indoc::formatdoc;

const TARGETS: &[(&str, &str)] = &[
    ("linux-arm", "arm-unknown-linux-gnueabi"),
    ("linux-arm64", "aarch64-unknown-linux-gnu"),
    ("linux-x64", "x86_64-unknown-linux-gnu"),
    ("linux-x86", "i686-unknown-linux-gnu"),
    ("macos-arm64", "aarch64-apple-darwin"),
    ("macos-x64", "x86_64-apple-darwin"),
];

const fn platform_for_target(target: &str) -> &str {
    let mut i = 0;
    while i < TARGETS.len() {
        if const_str::equal!(TARGETS[i].1, target) {
            return TARGETS[i].0;
        }
        i += 1;
    }
    target
}

fn main() {
    let out_dir = env::var_os("OUT_DIR").unwrap();
    let build_target = env::var_os("TARGET").unwrap();
    let metadata = MetadataCommand::new().exec().unwrap();
    let meta = metadata
        .root_package()
        .unwrap()
        .metadata
        .as_object()
        .unwrap();
    write_tree_sitter_consts(meta, &build_target, &out_dir);
    write_tsdl_consts(meta, &out_dir);
    let sha1 = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|str| format!(" ({})", str.trim()))
        .unwrap_or_default();
    fs::write(
        Path::new(&out_dir).join("tsdl.version"),
        format!("{}{}", env!("CARGO_PKG_VERSION"), sha1),
    )
    .unwrap();
}

fn write_tsdl_consts(meta: &serde_json::Map<String, serde_json::Value>, out_dir: &OsString) {
    let root = PathBuf::from(file!());
    let tsdl_bin_build_dir = root.parent().unwrap().join("src").canonicalize().unwrap();
    let tsdl_bin_build_dir = tsdl_bin_build_dir.to_str().unwrap();
    let tsdl = meta.get("tsdl").unwrap();
    let tsdl_build_dir = tsdl.get("build-dir").unwrap().as_str().unwrap();
    let tsdl_config_file = tsdl.get("config").unwrap().as_str().unwrap();
    let tsdl_fresh = tsdl.get("fresh").unwrap().as_bool().unwrap();
    let tsdl_from = tsdl.get("from").unwrap().as_str().unwrap();
    let tsdl_out_dir = tsdl.get("out").unwrap().as_str().unwrap();
    let tsdl_prefix = tsdl.get("prefix").unwrap().as_str().unwrap();
    let tsdl_ref = tsdl.get("ref").unwrap().as_str().unwrap();
    let tsdl_show_config = tsdl.get("show-config").unwrap().as_bool().unwrap();
    let tsdl_consts = Path::new(&out_dir).join("tsdl_consts.rs");
    fs::write(
        tsdl_consts,
        formatdoc!(
            r#"
              pub const TSDL_BIN_BUILD_DIR: &str = "{tsdl_bin_build_dir}/";
              pub const TSDL_BUILD_DIR: &str = "{tsdl_build_dir}";
              pub const TSDL_CONFIG_FILE: &str = "{tsdl_config_file}";
              pub const TSDL_FRESH: bool = {tsdl_fresh};
              pub const TSDL_FROM: &str = "{tsdl_from}";
              pub const TSDL_OUT_DIR: &str = "{tsdl_out_dir}";
              pub const TSDL_PREFIX: &str = "{tsdl_prefix}";
              pub const TSDL_REF: &str = "{tsdl_ref}";
              pub const TSDL_SHOW_CONFIG: bool = {tsdl_show_config};
            "#
        ),
    )
    .unwrap();
}

fn write_tree_sitter_consts(
    meta: &serde_json::Map<String, serde_json::Value>,
    build_target: &OsString,
    out_dir: &OsString,
) {
    let tree_sitter = meta.get("tree-sitter").unwrap();
    let tree_sitter_version = tree_sitter.get("version").unwrap().as_str().unwrap();
    let tree_sitter_repo = tree_sitter.get("repo").unwrap().as_str().unwrap();
    let tree_sitter_platform = platform_for_target(build_target.to_str().unwrap());
    let tree_sitter_consts = Path::new(out_dir).join("tree_sitter_consts.rs");
    fs::write(
        tree_sitter_consts,
        formatdoc!(
            r#"
              pub const TREE_SITTER_PLATFORM: &str = "{tree_sitter_platform}";
              pub const TREE_SITTER_REPO: &str = "{tree_sitter_repo}";
              pub const TREE_SITTER_VERSION: &str = "{tree_sitter_version}";
            "#
        ),
    )
    .unwrap();
}
