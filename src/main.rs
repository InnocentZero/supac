use std::fs;
use std::fs::read;
use std::io;
use std::path;

use backends::Backend;
use backends::Backends;
use backends::arch::Arch;
use backends::flatpak::Flatpak;
use clap::Args;
use clap::Parser;
use clap::Subcommand;
use env_logger::Env;
use log::error;
use log::info;
use parser::Engine;

mod backends;
mod commands;
mod config;
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
    Backends(BackendsCommand),
    CleanCache(CleanCacheCommand),
}

#[derive(Args)]
#[command(visible_alias("c"))]
/// remove unmanaged packages
struct CleanCommand {
    #[arg(short, long)]
    /// do not ask for any confirmation
    no_confirm: bool,
}

#[derive(Args)]
#[command(visible_alias("s"))]
/// install packages from groups
struct SyncCommand {
    #[arg(short, long)]
    /// do not ask for any confirmation
    no_confirm: bool,
}

#[derive(Args)]
#[command(visible_alias("u"))]
/// show explicitly installed packages not managed by metapac
struct UnmanagedCommand {}

#[derive(Args)]
#[command(visible_alias("b"))]
/// show the backends found by metapac
struct BackendsCommand {}

#[derive(Args)]
#[command(visible_alias("e"))]
/// clean the caches of all the backends, or the just those specified
struct CleanCacheCommand {
    backends: Option<Vec<String>>,
}

fn main() -> io::Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("off")).init();
    let args = Arguments::parse();

    let config_file = args
        .config_dir
        .map(|path| Ok(path))
        .unwrap_or_else(|| {
            info!("config path not supplied through arguments. Reading from default path");
            config::get_config_path()
        })
        .map_err(|e| {
            error!("Error encountered: {e:?}");
            io::ErrorKind::NotFound
        })?;

    if !config_file.exists() {
        fs::create_dir_all(config_file.parent().unwrap_or(path::Path::new("/"))).map_err(|e| {
            error!("Failed to create parent directories");
            error!(
                "While unlikely, it may be possible that their was no parent of the config file."
            );
            error!("{e:?}");
            io::ErrorKind::NotFound
        })?;

        fs::File::create(&config_file)
            .map_err(|e| {
                error!("Error occured: {e:?}");
                io::ErrorKind::NotFound
            })?
            .sync_all()
            .map_err(|e| {
                error!("Error occured: {e:?}");
                io::ErrorKind::InvalidData
            })?;

        config::write_default_config(&config_file).map_err(|e| {
            error!("Error occured while writing the default config.");
            error!("{e:?}");
            io::ErrorKind::InvalidInput
        })?;
    }

    let config_dir = config_file.parent().unwrap();

    let config_contents = fs::read(&config_file).map_err(|e| {
        error!("Error occured when reading the config spec");
        error!("{e:?}");
        e
    })?;
    let mut config_engine = Engine::new(&config_dir);
    let mut config = config_engine.fetch(&config_contents).map_err(|e| {
        error!("Error encountered while parsing config spec");
        error!("{e}");
        io::ErrorKind::InvalidData
    })?;

    let package_nu = [config_dir.as_os_str(), std::ffi::OsStr::new("package.nu")]
        .join(std::ffi::OsStr::new("/"));

    let contents = read(package_nu).map_err(|e| {
        error!("Error occured when reading the package spec.");
        error!("{e:?}");
        e
    })?;

    let mut engine = Engine::new(&config_dir);

    let packages = engine.fetch(&contents).map_err(|e| {
        error!("Error encountered while parsing package spec");
        error!("{e}");
        io::ErrorKind::InvalidData
    })?;

    let mut backends = parse_all_backends!(packages);

    let res = backends
        .iter_mut()
        .map(|backend_opt| {
            backend_opt.as_mut().map(|backend| match &args.subcommand {
                SubCommand::Clean(_clean_command) => backend.remove(&mut config),
                SubCommand::Sync(_sync_command) => backend.install(&mut engine, &mut config),
                SubCommand::Unmanaged(_unmanaged_command) => todo!(),
                SubCommand::Backends(_backends_command) => todo!(),
                SubCommand::CleanCache(_clean_cache_command) => todo!(),
            })
        })
        .flatten()
        .collect::<anyhow::Result<()>>();

    res.map_err(|e| {
        error!("Error encountered while processing command");
        error!("{e:?}");
        io::Error::from(io::ErrorKind::InvalidData)
    })
}
