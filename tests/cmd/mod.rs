#[cfg(test)]
mod build;
#[cfg(test)]
mod cache;
#[cfg(test)]
mod config;
#[cfg(test)]
mod log;
#[cfg(test)]
use std::{env, fs, path::Path};

use assert_cmd::{Command, cargo::cargo_bin_cmd};
use assert_fs::TempDir;

use tsdl::{args::BuildCommand, config as tsdl_config, consts::CONFIG_FILE};

pub struct Sandbox {
  pub build: BuildCommand,
  pub cmd: Command,
  pub tmp: TempDir,
}

impl Sandbox {
  pub fn new() -> Self {
    let tmp = TempDir::new().unwrap();
    let mut cmd = cargo_bin_cmd!();
    cmd.current_dir(tmp.path());
    Sandbox {
      build: BuildCommand::default(),
      cmd,
      tmp,
    }
  }

  pub fn config(&mut self, config: &str) -> &mut Self {
    self.config_at(config, &self.tmp.path().join(CONFIG_FILE))
  }

  pub fn config_at(&mut self, config_contents: &str, dst: &Path) -> &mut Self {
    fs::write(dst, config_contents).unwrap();
    self.build = tsdl_config::current(dst, None).unwrap();
    self
  }

  pub fn is_empty(&self) -> bool {
    fs::read_dir(&self.tmp).is_ok_and(|mut dir| dir.next().is_none())
  }
}
