use std::{env, ffi::OsString, path::PathBuf, sync::Mutex};

use anyhow::Result;
use assert_fs::prelude::*;
use indoc::{formatdoc, indoc};
#[cfg(test)]
use pretty_assertions::{assert_eq, assert_ne};

use tsdl::{
  args::{self, BuildCommand, Target},
  config::{self, Source},
  consts::{BUILD_DIR, FRESH, PARSER_OUT_DIR, PLATFORM, PREFIX, REPO, SHOW_CONFIG, VERSION},
};

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvVarGuard {
  key: &'static str,
  previous: Option<OsString>,
}

impl Drop for EnvVarGuard {
  fn drop(&mut self) {
    // SAFETY: The caller holds ENV_LOCK, serializing all env access
    // within these tests.
    match &self.previous {
      | None => unsafe { env::remove_var(self.key) },
      | Some(value) => unsafe { env::set_var(self.key, value) },
    }
  }
}

impl EnvVarGuard {
  fn set(key: &'static str, value: &str) -> Self {
    let previous = env::var_os(key);
    // SAFETY: The caller holds ENV_LOCK, serializing all env access
    // within these tests.
    unsafe { env::set_var(key, value) };
    Self { key, previous }
  }
}

fn current_with_cli(config: &assert_fs::fixture::ChildPath, argv: &[&str]) -> BuildCommand {
  let _lock = ENV_LOCK.lock().unwrap();
  let (_parsed_args, matches) = config::try_parse_from_with_matches(argv).unwrap();
  let build_matches = args::build_matches(&matches);
  config::current(config.path(), build_matches).unwrap()
}

fn current_with_cli_provenance(
  config: &assert_fs::fixture::ChildPath,
  argv: &[&str],
) -> (BuildCommand, config::BuildProvenance) {
  let _lock = ENV_LOCK.lock().unwrap();
  let (_parsed_args, matches) = config::try_parse_from_with_matches(argv).unwrap();
  let build_matches = args::build_matches(&matches);
  config::current_with_provenance(config.path(), build_matches).unwrap()
}

#[test]
fn boolean_env_can_override_config_file() -> Result<()> {
  let _lock = ENV_LOCK.lock().unwrap();
  let _force = EnvVarGuard::set("FORCE", "false");

  let temp = assert_fs::TempDir::new()?;
  let generated = temp.child("generated.toml");
  generated.write_str("force = true\n")?;

  let (_parsed_args, matches) = config::try_parse_from_with_matches(["tsdl", "build"]).unwrap();
  let build_matches = args::build_matches(&matches);
  let (cmd, prov) = config::current_with_provenance(generated.path(), build_matches)?;

  assert!(!cmd.force);
  assert_eq!(prov.force, Source::Environment);
  Ok(())
}

#[test]
fn cli_can_override_config_to_builtin_default_value() -> Result<()> {
  let temp = assert_fs::TempDir::new()?;
  let generated = temp.child("generated.toml");
  generated.write_str("target = \"wasm\"\n")?;

  let resolved = current_with_cli(&generated, &["tsdl", "build", "--target", "native"]);

  assert_eq!(resolved.target, Target::Native);
  Ok(())
}

#[test]
fn cli_can_override_tree_sitter_version_to_builtin_default_value() -> Result<()> {
  let temp = assert_fs::TempDir::new()?;
  let generated = temp.child("generated.toml");
  generated.write_str("[tree-sitter]\nversion = \"0.25.0\"\n")?;

  let resolved = current_with_cli(
    &generated,
    &["tsdl", "build", "--tree-sitter-version", VERSION],
  );

  assert_eq!(resolved.tree_sitter.version, VERSION);
  Ok(())
}

#[test]
fn cli_explicit_default_value_overrides_config_file() -> Result<()> {
  let temp = assert_fs::TempDir::new()?;
  let generated = temp.child("generated.toml");
  generated.write_str("build-dir = \"/custom\"\n")?;

  let (cmd, prov) =
    current_with_cli_provenance(&generated, &["tsdl", "build", "--build-dir", "tmp"]);

  assert_eq!(cmd.build_dir, PathBuf::from("tmp"));
  assert_eq!(prov.build_dir, Source::CommandLine);
  Ok(())
}

#[test]
fn cli_has_precedence_over_env() -> Result<()> {
  let _lock = ENV_LOCK.lock().unwrap();
  let _target = EnvVarGuard::set("TSDL_TARGET", "wasm");

  let temp = assert_fs::TempDir::new()?;
  let generated = temp.child("generated.toml");
  generated.touch()?;

  let (_parsed_args, matches) =
    config::try_parse_from_with_matches(["tsdl", "build", "--target", "native"]).unwrap();
  let build_matches = args::build_matches(&matches);
  let (cmd, prov) = config::current_with_provenance(generated.path(), build_matches)?;

  assert_eq!(cmd.target, Target::Native);
  assert_eq!(prov.target, Source::CommandLine);
  Ok(())
}

#[test]
fn current_default_is_default() -> Result<()> {
  let config_contents = formatdoc! {
    r#"
        build-dir = "{}"
        fresh = {}
        out-dir = "{}"
        show-config = {}

        [tree-sitter]
        git-ref = "{}"
        repo = "{}"
        platform = "{}"
      "#,
    BUILD_DIR,
    FRESH,
    PARSER_OUT_DIR,
    SHOW_CONFIG,
    VERSION,
    REPO,
    PLATFORM,
  };
  let temp = assert_fs::TempDir::new()?;
  let generated = temp.child("generated.toml");
  let def = BuildCommand::default();
  generated.write_str(&config_contents)?;
  assert_eq!(def, config::current(generated.path(), None).unwrap());
  assert_eq!(def, current_with_cli(&generated, &["tsdl", "build"]));
  Ok(())
}

#[test]
fn current_from_empty() -> Result<()> {
  let temp = assert_fs::TempDir::new()?;
  let generated = temp.child("generated.toml");
  let def = BuildCommand::default();
  generated.touch()?;
  assert_eq!(def, config::current(generated.path(), None).unwrap());
  Ok(())
}

#[test]
fn current_from_generated_default() -> Result<()> {
  let temp = assert_fs::TempDir::new()?;
  let generated = temp.child("generated.toml");
  let def = BuildCommand::default();
  generated.write_str(&toml::to_string(&def)?)?;
  assert_eq!(def, config::current(generated.path(), None).unwrap());
  Ok(())
}

#[test]
fn current_overrides_default() -> Result<()> {
  let config_contents = indoc! {
    r#"
        build-dir = "/root"
        fresh = true
        out-dir = "tree-sitter-parsers"
        show-config = true

        [tree-sitter]
        version = "1.0.0"
        repo = "https://gitlab.com/tree-sitter/tree-sitter"
        platform = "linux-arm64"
      "#
  };
  let temp = assert_fs::TempDir::new()?;
  let generated = temp.child("generated.toml");
  let def = BuildCommand::default();
  generated.write_str(config_contents)?;
  generated.assert(config_contents);
  assert_ne!(def, config::current(generated.path(), None).unwrap());
  assert_ne!(def, current_with_cli(&generated, &["tsdl", "build"]));
  Ok(())
}

#[test]
fn current_preserves_cli_languages() -> Result<()> {
  let temp = assert_fs::TempDir::new()?;
  let generated = temp.child("generated.toml");
  generated.touch()?;

  let def = BuildCommand::default();
  assert_eq!(def, current_with_cli(&generated, &["tsdl", "build"]));

  let mut expected = BuildCommand {
    languages: Some(vec!["rust".to_string()]),
    ..BuildCommand::default()
  };
  assert_eq!(
    expected,
    current_with_cli(&generated, &["tsdl", "build", "rust"])
  );

  expected.languages = Some(vec!["rust".to_string(), "ruby".to_string()]);
  assert_eq!(
    expected,
    current_with_cli(&generated, &["tsdl", "build", "rust", "ruby"])
  );
  Ok(())
}

#[test]
fn env_can_override_config_to_builtin_default_value() -> Result<()> {
  let _lock = ENV_LOCK.lock().unwrap();
  let _prefix = EnvVarGuard::set("PREFIX", PREFIX);

  let temp = assert_fs::TempDir::new()?;
  let generated = temp.child("generated.toml");
  generated.write_str("prefix = \"\"\n")?;

  let (_parsed_args, matches) = config::try_parse_from_with_matches(["tsdl", "build"]).unwrap();
  let build_matches = args::build_matches(&matches);
  let (cmd, prov) = config::current_with_provenance(generated.path(), build_matches)?;

  assert_eq!(cmd.prefix, PREFIX);
  assert_eq!(prov.prefix, Source::Environment);
  Ok(())
}

#[test]
fn negative_boolean_flag_overrides_positive() -> Result<()> {
  let temp = assert_fs::TempDir::new()?;
  let generated = temp.child("generated.toml");
  generated.touch()?;

  let (cmd, prov) =
    current_with_cli_provenance(&generated, &["tsdl", "build", "--force=true", "--no-force"]);

  assert!(!cmd.force);
  assert_eq!(prov.force, Source::CommandLine);
  Ok(())
}

#[test]
fn negative_boolean_flags_override_config_file() -> Result<()> {
  let temp = assert_fs::TempDir::new()?;
  let generated = temp.child("generated.toml");
  generated.write_str("force = true\nfresh = true\nshow-config = true\n")?;

  let resolved = current_with_cli(
    &generated,
    &[
      "tsdl",
      "build",
      "--no-force",
      "--no-fresh",
      "--no-show-config",
    ],
  );

  assert!(!resolved.force);
  assert!(!resolved.fresh);
  assert!(!resolved.show_config);
  Ok(())
}
