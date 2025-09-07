use std::{env, fs::File, io::Write, path::PathBuf};

use anyhow::{Context, Result};
use log::{error, trace};
use nu_protocol::{Record, Span};

// TODO: Change and implement using nu serde or json serde
const KV_PAIRS: [(&str, &str); 1] = [("arch_package_manager", "paru")];

pub fn get_config_path() -> Result<PathBuf> {
    let config_dir = if let Ok(config_dir) = env::var("SUPAC_HOME") {
        trace!("$SUPAC_HOME was defined. Using the value {config_dir}");
        Ok(config_dir)
    } else if let Ok(config_dir) = env::var("XDG_CONFIG") {
        trace!("$XDG_CONFIG was defined. Using the value {config_dir}");
        Ok(config_dir)
    } else if let Ok(home_dir) = env::var("HOME") {
        trace!("$HOME was defined. Using the value {home_dir}/.config");
        let config_dir = [home_dir.as_str(), ".config"].join("/");
        Ok(config_dir)
    } else {
        match env::var("USER") {
            Ok(user) => {
                trace!("$USER was defined. Using the value /home/{user}/.config");
                let config_dir = ["/home", user.as_str(), ".config"].join("/");
                Ok(config_dir)
            }
            Err(e) => {
                error!(
                    "None of the environment variables were defined to appropriately determine config directory."
                );
                Err(e)
            }
        }
    };

    Ok([config_dir?.as_str(), "supac", "config.nu"]
        .join("/")
        .into())
}

pub fn write_default_config(config_path: &PathBuf) -> Result<()> {
    let mut config_file = File::create(config_path).context(
        "
        Failed to create or open file. It might be a permissions issue, 
        since the relevant directories were created already.
        ",
    )?;

    let mut default_config = Record::new();
    KV_PAIRS.iter().for_each(|(k, v)| {
        default_config.insert(
            k.to_string(),
            nu_protocol::Value::String {
                val: v.to_string(),
                internal_span: Span::test_data(),
            },
        );
    });
    trace!("Built default config");

    let mut config_spec = String::with_capacity(30);
    config_spec.push_str("{\n");
    let _ = default_config
        .into_iter()
        .map(|(opt, value)| {
            config_spec.push_str(&opt);
            config_spec.push_str(": ");
            config_spec.push_str(value.as_str().unwrap());
            config_spec.push('\n');
        })
        .collect::<Vec<()>>();
    config_spec.push('}');

    config_file.write_all(config_spec.as_bytes()).context(
        "
        Failed to write the default config to the file.
        It could possibly be a permissions issue.
        ",
    )?;

    Ok(())
}
