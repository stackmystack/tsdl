use std::path::Path;

use diff::Diff;
use figment::{
    providers::{Format, Serialized, Toml},
    Figment,
};
use tracing::debug;

use crate::{
    args::{BuildCommand, ConfigCommand},
    error::TsdlError,
    git, TsdlResult,
};

pub fn run(command: &ConfigCommand, config: &Path) -> TsdlResult<()> {
    match command {
        ConfigCommand::Current => {
            let config: BuildCommand = current(config, None)?;
            println!(
                "{}",
                toml::to_string(&config)
                    .map_err(|e| { TsdlError::context("Generating default TOML config", e) })?
            );
        }
        ConfigCommand::Default => println!(
            "{}",
            toml::to_string(&BuildCommand::default())
                .map_err(|e| { TsdlError::context("Generating default TOML config", e) })?
        ),
    }
    Ok(())
}

pub fn current(config: &Path, command: Option<&BuildCommand>) -> TsdlResult<BuildCommand> {
    let from_default = BuildCommand::default();
    let mut from_file: BuildCommand = Figment::new()
        .merge(Serialized::defaults(from_default.clone()))
        .merge(Toml::file(config))
        .extract()
        .map_err(|e| TsdlError::context("Merging default and config file", e))?;
    match command {
        Some(from_command) => {
            debug!("Merging cli args + config files");
            let diff = from_default.diff(from_command);
            debug!("diff default command = {:?}", diff);
            from_file.apply(&diff);
        }
        None => {
            debug!("Skipping cli args + config file merger.");
        }
    }
    debug!("from_both = {:?}", from_file);
    // Figment is screwing with me, and it's overriding config coming
    // from Env::prefixed("TSDL_").
    // The scary thing is that I might have to write my own config
    // joiner, where I need to track provenance of the config, and also
    // whether it was explicitly set or taken from default … Figment
    // has many features I don't care about.
    Ok(from_file)
}

pub fn print_indent(s: &str, indent: &str) {
    s.lines().for_each(|line| println!("{indent}{line}"));
}

pub fn show(command: &BuildCommand) -> TsdlResult<()> {
    if let Some(langs) = &command.languages {
        println!("Building the following languages:");
        println!();
        println!(
            "{}",
            String::from_utf8(
                git::column(&langs.join(" "), "  ", 80)
                    .map_err(|e| TsdlError::context("Printing requested languages", e))?
                    .stdout
            )
            .map_err(|e| TsdlError::context(
                "Converting column-formatted languages to a string for printing",
                e
            ))?
        );
    } else {
        println!("Building all languages.");
        println!();
    }
    println!("Running with the following configuration:");
    println!();
    print_indent(
        &toml::to_string(&command).map_err(|e| TsdlError::context("Showing config", e))?,
        "  ",
    );
    println!();
    Ok(())
}
