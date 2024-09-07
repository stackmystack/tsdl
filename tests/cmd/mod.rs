#[cfg(test)]
mod build;
#[cfg(test)]
mod config;
#[cfg(test)]
mod log;

use std::{env, fs, path::Path};

use assert_cmd::Command;
use assert_fs::TempDir;
use figment::{
    providers::{Format, Serialized, Toml},
    Figment,
};

use tsdl::{args::BuildCommand, consts::TSDL_CONFIG_FILE};

pub struct Sandbox {
    pub build: BuildCommand,
    pub cmd: Command,
    pub tmp: TempDir,
}

impl Sandbox {
    pub fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let mut cmd = Command::cargo_bin(env!("CARGO_PKG_NAME")).unwrap();
        cmd.current_dir(tmp.path());
        Sandbox {
            build: BuildCommand::default(),
            cmd,
            tmp,
        }
    }

    pub fn config(&mut self, config: &str) -> &mut Self {
        self.config_at(config, &self.tmp.path().join(TSDL_CONFIG_FILE))
    }

    pub fn config_at(&mut self, config: &str, dst: &Path) -> &mut Self {
        self.build = Figment::new()
            .merge(Serialized::defaults(BuildCommand::default()))
            .merge(Toml::string(config))
            .extract()
            .unwrap();
        fs::write(dst, config).unwrap();
        self
    }

    pub fn is_empty(&self) -> bool {
        fs::read_dir(&self.tmp).is_ok_and(|mut dir| dir.next().is_none())
    }
}
