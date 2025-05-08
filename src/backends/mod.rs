use anyhow::Result;
use arch::Arch;
use flatpak::Flatpak;
use nu_protocol::Record;

use crate::parser::Engine;

pub mod arch;
pub mod flatpak;

pub enum Backends {
    Arch(Arch),
    Flatpak(Flatpak),
}

pub trait Backend {
    fn install(&mut self, engine: &mut Engine, config: &mut Record) -> Result<()>;
    fn new(value: &Record) -> Result<Self>
    where
        Self: Sized;
    fn remove(&mut self, config: &mut Record) -> Result<()>;
}

impl Backends {
    pub fn install(&mut self, engine: &mut Engine, config: &mut Record) -> Result<()> {
        match self {
            Backends::Arch(arch) => arch.install(engine, config),
            Backends::Flatpak(flatpak) => flatpak.install(engine, config),
        }
    }

    pub fn remove(&mut self, config: &mut Record) -> Result<()> {
        match self {
            Backends::Arch(arch) => arch.remove(config),
            Backends::Flatpak(flatpak) => flatpak.remove(config),
        }
    }
}

#[macro_export]
macro_rules! backend_parse {
    ($packages:ident, $($backend:ident),*) => {
        [$(
            {let packages = $packages
                .get(stringify!($backend))
                .map(|package_struct| package_struct.as_record().ok())
                .flatten();

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
        backend_parse!($packages, Arch, Flatpak)
    };
}
