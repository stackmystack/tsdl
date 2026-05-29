//! Build-time constants generated from `Cargo.toml` metadata.

// Include the generated constants.
// See build.rs .
include!(concat!(env!("OUT_DIR"), "/tree_sitter_consts.rs"));
include!(concat!(env!("OUT_DIR"), "/tsdl_consts.rs"));
