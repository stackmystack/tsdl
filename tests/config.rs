use assert_fs::prelude::*;
use indoc::{formatdoc, indoc};
use miette::{IntoDiagnostic, Result};
#[cfg(test)]
use pretty_assertions::{assert_eq, assert_ne};

use tsdl::{
    args::BuildCommand,
    config,
    consts::{
        TREE_SITTER_PLATFORM, TREE_SITTER_REPO, TREE_SITTER_VERSION, TSDL_BUILD_DIR, TSDL_FRESH,
        TSDL_OUT_DIR, TSDL_SHOW_CONFIG,
    },
};

#[test]
fn current_from_generated_default() -> Result<()> {
    let temp = assert_fs::TempDir::new().into_diagnostic()?;
    let generated = temp.child("generated.toml");
    let def = BuildCommand::default();
    generated
        .write_str(&toml::to_string(&def).into_diagnostic()?)
        .into_diagnostic()?;
    assert_eq!(def, config::current(&generated, None).unwrap());
    Ok(())
}

#[test]
fn current_from_empty() -> Result<()> {
    let temp = assert_fs::TempDir::new().into_diagnostic()?;
    let generated = temp.child("generated.toml");
    let def = BuildCommand::default();
    generated.touch().into_diagnostic()?;
    assert_eq!(def, config::current(&generated, None).unwrap());
    Ok(())
}

#[test]
fn current_preserve_languages() -> Result<()> {
    let temp = assert_fs::TempDir::new().into_diagnostic()?;
    let generated = temp.child("generated.toml");
    let mut def = BuildCommand::default();
    generated.touch().into_diagnostic()?;
    def.languages = None;
    assert_eq!(def, config::current(&generated, Some(&def)).unwrap());
    def.languages = Some(vec![]);
    assert_eq!(def, config::current(&generated, Some(&def)).unwrap());
    def.languages = Some(vec!["rust".to_string()]);
    assert_eq!(def, config::current(&generated, Some(&def)).unwrap());
    def.languages = Some(vec!["rust".to_string(), "ruby".to_string()]);
    assert_eq!(def, config::current(&generated, Some(&def)).unwrap());
    Ok(())
}

#[test]
fn current_default_is_default() -> Result<()> {
    let config = formatdoc! {
      r#"
        build-dir = "{}"
        fresh = {}
        out = "{}"
        show-config = {}

        [tree-sitter]
        version = "{}"
        repo = "{}"
        platform = "{}"
      "#,
      TSDL_BUILD_DIR,
      TSDL_FRESH,
      TSDL_OUT_DIR,
      TSDL_SHOW_CONFIG,
      TREE_SITTER_VERSION,
      TREE_SITTER_REPO,
      TREE_SITTER_PLATFORM,
    };
    let temp = assert_fs::TempDir::new().into_diagnostic()?;
    let generated = temp.child("generated.toml");
    let def = BuildCommand::default();
    generated.write_str(&config).into_diagnostic()?;
    assert_eq!(def, config::current(&generated, None).unwrap());
    assert_eq!(def, config::current(&generated, Some(&def)).unwrap());
    Ok(())
}

#[test]
fn current_overrides_default() -> Result<()> {
    let config = indoc! {
      r#"
        build-dir = "/root"
        fresh = true
        out = "tree-sitter-parsers"
        show-config = true

        [tree-sitter]
        version = "1.0.0"
        repo = "https://gitlab.com/tree-sitter/tree-sitter"
        platform = "linux-arm64"
      "#
    };
    let temp = assert_fs::TempDir::new().into_diagnostic()?;
    let generated = temp.child("generated.toml");
    let def = BuildCommand::default();
    generated.write_str(config).into_diagnostic()?;
    generated.assert(config);
    assert_ne!(def, config::current(&generated, None).unwrap());
    assert_ne!(def, config::current(&generated, Some(&def)).unwrap());
    Ok(())
}
