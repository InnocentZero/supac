use anyhow::Result;
use nu_protocol::Record;

use crate::parser::Engine;

pub mod arch;

#[macro_export]
macro_rules! backend_parse {
    ($packages:ident, $($backend:ident),*) => {
        [$(
            {let packages = $packages
            .get(stringify!($backend))
            .map(|package_struct| package_struct.as_record().ok())
            .unwrap_or(None);

            match packages {
                Some(packages) =>
                Some(
                    $backend::new(packages)
                    .map_err(|e| {
                        error!("Error encountered in parsing {} packages", stringify!($backend));
                        error!("{e}");
                        io::ErrorKind::InvalidData
                    })?
                ),
                None => None,
            }}

        )*]
    };
}

#[macro_export]
macro_rules! parse_all_backends {
    ($packages:ident) => {
        backend_parse!($packages, Arch)
    }
}

pub trait Backend {
    fn new(value: &Record) -> Result<Self>
    where
        Self: Sized;
    fn install(&mut self, engine: &mut Engine) -> Result<()>;
}
