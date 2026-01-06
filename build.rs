use cargo_metadata::MetadataCommand;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

/// Maps targets to Tree Sitter platform strings.
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

/// Generates a Rust file with `pub const` definitions.
///
/// Supports two sources:
/// 1. `json(object, "key")`: Extracts value from `serde_json` Map.
/// 2. `expr(value)`: Uses a raw Rust expression.
macro_rules! generate_consts {
    ($path:expr, $( $name:ident : $type:ident = $source:ident ( $($args:expr),* ) ),* $(,)?) => {
        {
            let mut buf = String::new();
            $(
                generate_consts!(@expand buf, $name, $type, $source($($args),*));
            )*
            std::fs::write($path, buf).expect("Failed to write consts file");
        }
    };

    // Case: JSON String
    (@expand $buf:expr, $name:ident, str, json($obj:expr, $key:literal)) => {
        let val = $obj.get($key).expect(concat!("Key not found: ", $key))
                      .as_str().expect(concat!("Key not a string: ", $key));
        writeln!($buf, "pub const {}: &str = {:?};", stringify!($name), val).unwrap();
    };

    // Case: JSON Bool
    (@expand $buf:expr, $name:ident, bool, json($obj:expr, $key:literal)) => {
        let val = $obj.get($key).expect(concat!("Key not found: ", $key))
                      .as_bool().expect(concat!("Key not a bool: ", $key));
        writeln!($buf, "pub const {}: bool = {};", stringify!($name), val).unwrap();
    };

    // Case: Raw Expression (String)
    (@expand $buf:expr, $name:ident, str, expr($val:expr)) => {
        writeln!($buf, "pub const {}: &str = {:?};", stringify!($name), $val).unwrap();
    };
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");

    let out_dir = env::var_os("OUT_DIR").unwrap();
    let build_target = env::var("TARGET").unwrap();

    // 1. Get Metadata
    let metadata = MetadataCommand::new().exec().unwrap();
    let meta = metadata
        .root_package()
        .unwrap()
        .metadata
        .as_object()
        .unwrap();

    // 2. Prep dynamic values
    let tsdl_bin_build_dir = PathBuf::from(file!())
        .parent()
        .unwrap()
        .join("src")
        .canonicalize()
        .unwrap()
        .join(""); // Ensure trailing slash logic if needed, or handle in string

    // Note: User original code added a trailing slash via format string,
    // we convert to string here for the macro.
    let tsdl_bin_str = format!("{}/", tsdl_bin_build_dir.to_str().unwrap());

    let ts_platform = platform_for_target(&build_target);

    // 3. Generate TSDL Consts
    let tsdl = meta.get("tsdl").expect("missing [metadata.tsdl]");
    generate_consts!(
        Path::new(&out_dir).join("tsdl_consts.rs"),
        TSDL_BIN_BUILD_DIR : str  = expr(tsdl_bin_str),
        TSDL_BUILD_DIR     : str  = json(tsdl, "build-dir"),
        TSDL_CACHE_FILE    : str  = json(tsdl, "cache-file"),
        TSDL_CONFIG_FILE   : str  = json(tsdl, "config-file"),
        TSDL_FORCE         : bool = json(tsdl, "force"),
        TSDL_FRESH         : bool = json(tsdl, "fresh"),
        TSDL_FROM          : str  = json(tsdl, "from"),
        TSDL_LOCK_FILE     : str  = json(tsdl, "lock-file"),
        TSDL_OUT_DIR       : str  = json(tsdl, "out-dir"),
        TSDL_PREFIX        : str  = json(tsdl, "prefix"),
        TSDL_REF           : str  = json(tsdl, "ref"),
        TSDL_SHOW_CONFIG   : bool = json(tsdl, "show-config"),
    );

    // 4. Generate Tree Sitter Consts
    let tree_sitter = meta
        .get("tree-sitter")
        .expect("missing [metadata.tree-sitter]");
    generate_consts!(
        Path::new(&out_dir).join("tree_sitter_consts.rs"),
        TREE_SITTER_PLATFORM : str = expr(ts_platform),
        TREE_SITTER_REPO     : str = json(tree_sitter, "repo"),
        TREE_SITTER_REF      : str = json(tree_sitter, "ref"),
    );

    // 5. Generate Version/SHA
    let sha1 = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| format!(" ({})", s.trim()))
        .unwrap_or_default();

    fs::write(
        Path::new(&out_dir).join("tsdl.version"),
        format!("{}{}", env!("CARGO_PKG_VERSION"), sha1),
    )
    .unwrap();
}
