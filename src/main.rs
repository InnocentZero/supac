use std::ffi::OsStr;
use std::fs::{File, create_dir_all, read};
use std::path;

use anyhow::anyhow;
use backends::Arch;
use backends::Backend;
use backends::Backends;
use backends::Cargo;
use backends::Flatpak;
use backends::Rustup;
use clap::Args;
use clap::Parser;
use clap::Subcommand;
use env_logger::Env;
use parser::Engine;

mod backends;
mod commands;
mod config;
mod error;
mod parser;

/// A nushell based declarative package management utility
#[derive(Parser)]
#[command(version, about)]
struct Arguments {
    /// Path to config-file
    #[arg(short, long)]
    config_dir: Option<path::PathBuf>,
    #[command(subcommand)]
    subcommand: SubCommand,
}

#[derive(Subcommand)]
enum SubCommand {
    Clean(CleanCommand),
    Sync(SyncCommand),
    Unmanaged(UnmanagedCommand),
    Validate(ValidateCommand),
    CleanCache(CleanCacheCommand),
}

#[derive(Args)]
#[command(visible_alias("c"))]
/// remove unmanaged packages
struct CleanCommand {
    #[arg(short = 'n', long)]
    /// do not execute commands
    dry_run: bool,
    #[arg(short = 'y', long)]
    /// do not ask for any confirmation
    no_confirm: bool,
}

#[derive(Args)]
#[command(visible_alias("s"))]
/// install packages from groups
struct SyncCommand {
    #[arg(short = 'n', long)]
    /// do not execute commands
    dry_run: bool,
    #[arg(short = 'y', long)]
    /// do not ask for any confirmation
    no_confirm: bool,
}

#[derive(Args)]
#[command(visible_alias("u"))]
/// show explicitly installed packages not managed
struct UnmanagedCommand;

#[derive(Args)]
#[command(visible_alias("b"))]
/// show the backends found and validate their configs
struct ValidateCommand;

#[derive(Args)]
#[command(visible_alias("e"))]
/// clean the caches of all the backends
struct CleanCacheCommand {
    #[arg(short = 'n', long)]
    /// do not execute commands
    dry_run: bool,
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("off")).init();
    let args = Arguments::parse();

    let config_file = args
        .config_dir
        .map(Ok)
        .unwrap_or_else(|| {
            log::info!("config path not supplied through arguments. Reading from default path");
            config::get_config_path()
        })
        .map_err(|e| mod_err!(e))?;

    if !config_file.exists() {
        create_dir_all(config_file.parent().unwrap_or(path::Path::new("/"))).map_err(|e| {
            log::error!("Failed to create parent directories");
            log::error!(
                "While unlikely, it may be possible that their was no parent of the config file."
            );
            mod_err!(e)
        })?;

        File::create(&config_file)
            .map_err(|e| mod_err!(e))?
            .sync_all()
            .map_err(|e| mod_err!(e))?;

        config::write_default_config(&config_file).map_err(|e| {
            log::error!("Error occured while writing the default config.");
            mod_err!(e)
        })?;
    }

    let config_dir = config_file.parent().unwrap();

    let config_contents = read(&config_file).map_err(|e| {
        log::error!("Error occured when reading the config spec");
        log::error!("{e:?}");
        e
    })?;
    let mut config_engine = Engine::new(config_dir);
    let config = config_engine.fetch(&config_contents).map_err(|e| {
        log::error!("Error encountered while parsing config spec");
        mod_err!(e)
    })?;

    let package_nu = [config_dir.as_os_str(), OsStr::new("package.nu")].join(OsStr::new("/"));

    let contents = read(package_nu).map_err(|e| {
        log::error!("Error occured when reading the package spec.");
        log::error!("{e:?}");
        e
    })?;

    let mut engine = Engine::new(config_dir);

    let packages = engine
        .fetch(&contents)
        .map_err(|e| nest_errors!("Error encountered while parsing package spec", e))?;

    let mut backends = parse_all_backends!(packages, config);

    let results = backends.iter_mut().flat_map(|backend_opt| {
        backend_opt.as_mut().map(|backend| match &args.subcommand {
            SubCommand::Clean(clean_command) => backend.remove(clean_command),
            SubCommand::Sync(sync_command) => backend.install(&mut engine, sync_command),
            SubCommand::Unmanaged(_unmanaged_command) => todo!("Not implemented yet"),
            SubCommand::Validate(_validate_command) => todo!("Not implemented yet"),
            SubCommand::CleanCache(clean_cache_command) => {
                backend.clean_cache(&config, clean_cache_command)
            }
        })
    });

    #[allow(clippy::manual_try_fold)]
    results.fold(Ok(()), |acc, curr| match (acc, curr) {
        (acc, Ok(_)) => acc,
        (Ok(_), curr) => curr,
        (Err(orig), Err(e)) => Err(concat_err!(orig, e)),
    })
}
