use anyhow::Result;
pub use arch::Arch;
pub use cargo::Cargo;
pub use flatpak::Flatpak;
pub use rustup::Rustup;
use nu_protocol::Record;

use crate::parser::Engine;

mod arch;
mod cargo;
mod flatpak;
mod rustup;

#[derive(Debug)]
pub enum Backends {
    Arch(Arch),
    Flatpak(Flatpak),
    Cargo(Cargo),
}

pub trait Backend {
    fn clean_cache(&self, config: &Record) -> Result<()>;
    fn install(&self, engine: &mut Engine, config: &mut Record) -> Result<()>;
    fn new(value: &Record) -> Result<Self>
    where
        Self: Sized;
    fn remove(&self, config: &mut Record) -> Result<()>;
}

impl Backends {
    pub fn install(&mut self, engine: &mut Engine, config: &mut Record) -> Result<()> {
        match self {
            Backends::Arch(arch) => arch.install(engine, config),
            Backends::Flatpak(flatpak) => flatpak.install(engine, config),
            Backends::Cargo(cargo) => cargo.install(engine, config),
        }
    }

    pub fn remove(&mut self, config: &mut Record) -> Result<()> {
        match self {
            Backends::Arch(arch) => arch.remove(config),
            Backends::Flatpak(flatpak) => flatpak.remove(config),
            Backends::Cargo(cargo) => cargo.remove(config),
        }
    }

    pub fn clean_cache(&mut self, config: &Record) -> Result<()> {
        match self {
            Backends::Arch(arch) => arch.clean_cache(config),
            Backends::Flatpak(flatpak) => flatpak.clean_cache(config),
            Backends::Cargo(cargo) => cargo.clean_cache(config),
        }
    }
}

#[macro_export]
macro_rules! backend_parse {
    ($packages:ident, $($backend:ident),*) => {
        [$(
            {let packages = $packages
                .get(stringify!($backend))
                .and_then(|package_struct| package_struct.as_record().ok());

            match packages {
                Some(packages) =>
                Some(
                    Backends::$backend($backend::new(packages)
                    .map_err(|e| {
                        error!("Error encountered in parsing {} packages", stringify!($backend));
                        error!("{e}");
                        io::ErrorKind::InvalidData
                    })?)
                ),
                None => None,
            }},

        )*]
    };
}

#[macro_export]
macro_rules! parse_all_backends {
    ($packages:ident) => {
        backend_parse!($packages, Arch, Flatpak, Cargo)
    };
}
