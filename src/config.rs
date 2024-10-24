use std::path::Path;

use diff::Diff;
use figment::{
    providers::{Format, Serialized, Toml},
    Figment,
};
use miette::{Context, IntoDiagnostic, Result};
use tracing::debug;

use crate::{
    args::{BuildCommand, ConfigCommand},
    git,
};

pub fn run(command: &ConfigCommand, config: &Path) -> Result<()> {
    match command {
        ConfigCommand::Current => {
            let config: BuildCommand = current(config, None)?;
            println!(
                "{}",
                toml::to_string(&config)
                    .into_diagnostic()
                    .wrap_err("Generating default TOML config")?
            );
        }
        ConfigCommand::Default => println!(
            "{}",
            toml::to_string(&BuildCommand::default()).into_diagnostic()?
        ),
    };
    Ok(())
}

pub fn current(config: &Path, command: Option<&BuildCommand>) -> Result<BuildCommand> {
    let from_default = BuildCommand::default();
    let mut from_file: BuildCommand = Figment::new()
        .merge(Serialized::defaults(from_default.clone()))
        .merge(Toml::file(config))
        .extract()
        .into_diagnostic()
        .wrap_err("Merging default and config file")?;
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
    };
    debug!("from_both = {:?}", from_file);
    // Figment is screwing with me, and it's overrinding config coming
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

pub fn show(command: &BuildCommand) -> Result<()> {
    if let Some(langs) = &command.languages {
        println!("Building the following languages:");
        println!();
        println!(
            "{}",
            String::from_utf8(
                git::column(&langs.join(" "), "  ", 80)
                    .wrap_err("Printing requested languages")?
                    .stdout
            )
            .into_diagnostic()
            .wrap_err("Converting column-formatted languages to a string for printing")?
        );
    } else {
        println!("Building all languages.");
        println!();
    }
    println!("Running with the following configuration:");
    println!();
    print_indent(
        &toml::to_string(&command)
            .into_diagnostic()
            .wrap_err("Showing config")?,
        "  ",
    );
    println!();
    Ok(())
}
