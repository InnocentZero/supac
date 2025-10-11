use std::{env, fs::File, io::Write, path::PathBuf};

use anyhow::{Result, anyhow};

use crate::{function, mod_err, nest_errors};

pub const ARCH_PACKAGE_MANAGER_KEY: &str = "arch_package_manager";
pub const DEFAULT_PACKAGE_MANAGER: &str = "paru";

pub const FLATPAK_DEFAULT_SYSTEMWIDE_KEY: &str = "flatpak_default_systemwide";
pub const DEFAULT_FLATPAK_SYSTEMWIDE: bool = false;

const CONFIG: [(&str, &str); 2] = [
    (ARCH_PACKAGE_MANAGER_KEY, DEFAULT_PACKAGE_MANAGER),
    (FLATPAK_DEFAULT_SYSTEMWIDE_KEY, "false"),
];

pub fn get_config_path() -> Result<PathBuf> {
    let config_dir = if let Ok(config_dir) = env::var("SUPAC_HOME") {
        log::trace!("$SUPAC_HOME was defined. Using the value {config_dir}");
        Ok(config_dir)
    } else if let Ok(config_dir) = env::var("XDG_CONFIG") {
        log::trace!("$XDG_CONFIG was defined. Using the value {config_dir}");
        Ok(config_dir)
    } else if let Ok(home_dir) = env::var("HOME") {
        log::trace!("$HOME was defined. Using the value {home_dir}/.config");
        let config_dir = [home_dir.as_str(), ".config"].join("/");
        Ok(config_dir)
    } else {
        match env::var("USER") {
            Ok(user) => {
                log::trace!("$USER was defined. Using the value /home/{user}/.config");
                let config_dir = ["/home", user.as_str(), ".config"].join("/");
                Ok(config_dir)
            }
            Err(_) => Err(mod_err!(
                "None of the environment variables were defined to appropriately determine config directory."
            )),
        }
    };

    Ok([config_dir?.as_str(), "supac", "config.nu"]
        .join("/")
        .into())
}

pub fn write_default_config(config_path: &PathBuf) -> Result<()> {
    let mut config_file = File::create(config_path).map_err(|e| {
        nest_errors!(
            concat!(
                "Failed to create or open file.",
                "It might be a permissions issue",
                " since the relevant directories were created already."
            ),
            e
        )
    })?;

    let mut config_spec = "{\n".to_owned();
    CONFIG.iter().for_each(|(k, v)| {
        config_spec.push_str(("    ".to_owned() + *k + " : " + v + ",\n").as_str());
    });
    config_spec.push_str("}\n");

    log::trace!("Built default config");

    config_file.write_all(config_spec.as_bytes()).map_err(|e| {
        nest_errors!(
            concat!(
                "Failed to write the default config to the file.",
                "It could possibly be a permissions issue.",
            ),
            e
        )
    })?;

    Ok(())
}
