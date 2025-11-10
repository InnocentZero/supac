use anyhow::Result;
pub use arch::Arch;
pub use cargo::Cargo;
pub use flatpak::Flatpak;
use nu_protocol::Record;
pub use rustup::Rustup;

use crate::{CleanCacheCommand, CleanCommand, SyncCommand, parser::Engine};

mod arch;
mod cargo;
mod flatpak;
mod rustup;

#[derive(Debug)]
pub enum Backends {
    Arch(Arch),
    Flatpak(Flatpak),
    Cargo(Cargo),
    Rustup(Rustup),
}

pub trait Backend {
    fn clean_cache(&self, config: &Record, opts: &CleanCacheCommand) -> Result<()>;
    fn install(&self, engine: &mut Engine, opts: &SyncCommand) -> Result<()>;
    fn new(value: &Record, config: &Record) -> Result<Self>
    where
        Self: Sized;
    fn remove(&self, opts: &CleanCommand) -> Result<()>;
}

impl Backends {
    pub fn install(&mut self, engine: &mut Engine, opts: &SyncCommand) -> Result<()> {
        match self {
            Backends::Arch(arch) => arch.install(engine, opts),
            Backends::Flatpak(flatpak) => flatpak.install(engine, opts),
            Backends::Cargo(cargo) => cargo.install(engine, opts),
            Backends::Rustup(rustup) => rustup.install(engine, opts),
        }
    }

    pub fn remove(&mut self, opts: &CleanCommand) -> Result<()> {
        match self {
            Backends::Arch(arch) => arch.remove(opts),
            Backends::Flatpak(flatpak) => flatpak.remove(opts),
            Backends::Cargo(cargo) => cargo.remove(opts),
            Backends::Rustup(rustup) => rustup.remove(opts),
        }
    }

    pub fn clean_cache(&mut self, config: &Record, opts: &CleanCacheCommand) -> Result<()> {
        match self {
            Backends::Arch(arch) => arch.clean_cache(config, opts),
            Backends::Flatpak(flatpak) => flatpak.clean_cache(config, opts),
            Backends::Cargo(cargo) => cargo.clean_cache(config, opts),
            Backends::Rustup(rustup) => rustup.clean_cache(config, opts),
        }
    }
}

#[macro_export]
macro_rules! backend_parse {
    ($packages:ident, $config:ident, $($backend:ident),*) => {
        [$(
            {let packages = $packages
                .get(stringify!($backend))
                .and_then(|package_struct| package_struct.as_record().ok());

            match packages {
                Some(packages) =>
                Some(
                    Backends::$backend($backend::new(packages, &$config)
                    .map_err(|e| {
                        log::error!("Error encountered in parsing {} packages", stringify!($backend));
                        mod_err!(e)
                    })?)
                ),
                None => None,
            }},

        )*]
    };
}

#[macro_export]
macro_rules! parse_all_backends {
    ($packages:ident, $config:ident) => {
        backend_parse!($packages, $config, Arch, Flatpak, Cargo, Rustup)
    };
}
