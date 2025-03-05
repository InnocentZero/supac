use std::fs;
use std::fs::read;
use std::io;
use std::path;

mod backends;
mod commands;
mod config;
mod parser;

use anyhow::anyhow;
use backends::Backend;
use backends::arch::Arch;
use clap::Parser;
use env_logger::Env;
use log::error;
use log::info;
use parser::Engine;

/// A nushell based declarative package management utility
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Path to config-file
    #[arg(short, long)]
    config: Option<path::PathBuf>,
}

fn main() -> io::Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("off")).init();
    let args = Args::parse();

    let config_file = args
        .config
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
    let config = config_engine.fetch(&config_contents).map_err(|e| {
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
        .map(|backend| backend.as_mut().map(|arch| arch.install(&mut engine)))
        .map(|backend| backend.unwrap_or(Err(anyhow!("Failed to install package"))))
        .collect::<anyhow::Result<()>>();

    Ok(())
}
