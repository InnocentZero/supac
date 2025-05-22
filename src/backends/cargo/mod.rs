use std::collections::{HashMap, HashSet};
use std::fs;

use anyhow::{Result, anyhow};
use nu_protocol::{Record, engine::Closure};

use crate::commands::{Perms, run_command};
use crate::parser::Engine;

use super::Backend;

#[derive(Clone, Debug)]
pub struct CargoOpts {
    features: Box<[String]>,
    all_features: bool,
    no_default_features: bool,
    git_remote: Option<String>,
    post_hook: Option<Closure>,
}

#[derive(Clone, Debug)]
pub struct Cargo {
    packages: HashMap<String, CargoOpts>,
}

impl Backend for Cargo {
    fn new(value: &Record) -> Result<Self> {
        let packages = value
            .get("packages")
            .ok_or(anyhow!("Failed to get packages for Arch"))?
            .as_list()?
            .iter()
            .map(value_to_pkgspec)
            .collect::<Result<_>>()?;

        Ok(Cargo { packages })
    }

    fn install(&mut self, engine: &mut Engine, _config: &mut Record) -> Result<()> {
        let packages = get_installed_packages();
        let configured_packages = &self.packages;
        let missing_packages = configured_packages
            .iter()
            .filter(|(name, _)| !packages.contains(*name));

        let mut post_hooks = Vec::new();

        missing_packages
            .map(|(name, spec)| {
                spec.post_hook.as_ref().map(|hook| post_hooks.push(hook));
                install_package(name, spec)
            })
            .collect::<Result<()>>()?;

        post_hooks
            .into_iter()
            .map(|hook| engine.execute_closure(hook))
            .collect()
    }

    fn remove(&mut self, _config: &mut Record) -> Result<()> {
        let packages = get_installed_packages();
        let configured_packages = &self.packages;
        packages
            .into_iter()
            .filter(|package| !configured_packages.contains_key(package))
            .map(|package| run_command(["cargo", "uninstall", package.as_str()], Perms::User))
            .collect()
    }
}

fn value_to_pkgspec(value: &nu_protocol::Value) -> Result<(String, CargoOpts)> {
    let record = value.as_record()?;

    let package = record
        .get("package")
        .ok_or(anyhow!("No package mentioned"))?
        .as_str()?
        .to_owned();

    let all_features = record
        .get("all_features")
        .map(|value| value.as_bool().ok())
        .flatten()
        .unwrap_or(false);

    let no_default_features = record
        .get("no_default_features")
        .map(|value| value.as_bool().ok())
        .flatten()
        .unwrap_or(false);

    let features = if all_features || no_default_features {
        Box::new([])
    } else {
        record
            .get("features")
            .map(|value| value.as_list().ok())
            .flatten()
            .map(|list| {
                list.iter()
                    .filter_map(|elem| elem.as_str().ok())
                    .map(ToOwned::to_owned)
                    .collect::<Box<[_]>>()
            })
            .unwrap_or(Box::new([]))
    };

    let git_remote = record
        .get("git_remote")
        .map(|value| value.as_str().ok())
        .flatten()
        .map(ToOwned::to_owned);

    let post_hook = record
        .get("post_hook")
        .map(|closure| closure.as_closure().ok())
        .flatten()
        .map(|post_hook| {
            if !post_hook.captures.is_empty() {
                None
            } else {
                Some(post_hook.to_owned())
            }
        })
        .flatten();

    Ok((
        package,
        CargoOpts {
            features,
            no_default_features,
            all_features,
            git_remote,
            post_hook,
        },
    ))
}

fn get_installed_packages() -> HashSet<String> {
    let crate_file =
        std::env::var("CARGO_HOME").unwrap_or("~/.cargo".to_owned()) + "/.crates2.json";

    let cratespec = match fs::read_to_string(&crate_file) {
        Ok(cratespec) => cratespec,
        Err(_) => return HashSet::new(),
    };

    let cratespec: serde_json::Value = match serde_json::from_str(&cratespec) {
        Ok(cratespec) => cratespec,
        Err(_) => return HashSet::new(),
    };

    let packages: HashSet<_> = cratespec
        .get("installs")
        .map(|value| value.as_object())
        .flatten()
        .map(|value| {
            value
                .keys()
                .filter_map(|package| package.split_once(' ').map(|package| package.0))
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or(HashSet::new());

    packages
}

fn install_package(name: &str, spec: &CargoOpts) -> Result<()> {
    run_command(
        ["cargo", "install"]
            .into_iter()
            .chain(
                Some("--git")
                    .into_iter()
                    .filter(|_| spec.git_remote.is_some()),
            )
            .chain(spec.git_remote.as_deref())
            .chain(
                Some("--all-features")
                    .into_iter()
                    .filter(|_| spec.all_features),
            )
            .chain(
                Some("--no-default-features")
                    .into_iter()
                    .filter(|_| spec.no_default_features),
            )
            .chain(
                Some("--features")
                    .into_iter()
                    .filter(|_| !spec.features.is_empty()),
            )
            .chain(spec.features.iter().map(String::as_str))
            .chain([name]),
        Perms::User,
    )
}

fn _cargospec_to_pkgspec(name: &str, spec: &serde_json::Value) -> Result<(String, CargoOpts)> {
    let spec = spec.as_object().ok_or(anyhow!("Malformed spec: {name}"))?;

    let (name, version_source) = name
        .split_once(' ')
        .ok_or(anyhow!("Malformed name: {name}"))?;

    let (_version, source) = version_source
        .split_once(' ')
        .ok_or(anyhow!("Malformed version/source: {name}"))?;

    let git_remote = if source.starts_with("(git+") {
        let url = source
            .split("+")
            .nth(1)
            .ok_or(anyhow!("Malformed git source: {name}"))?
            .split("#")
            .next()
            .ok_or(anyhow!("Malformed git url: {name}"))?
            .to_owned();

        Some(url)
    } else {
        None
    };

    let all_features = spec
        .get("all_features")
        .ok_or(anyhow!("Missing field all_features: {name}"))?
        .as_bool()
        .ok_or(anyhow!("Malformed field all_features not a bool: {name}"))?;

    let no_default_features = spec
        .get("no_default_features")
        .ok_or(anyhow!("Missing field all_features: {name}"))?
        .as_bool()
        .ok_or(anyhow!("Malformed field all_features not a bool: {name}"))?;

    let features = spec
        .get("features")
        .ok_or(anyhow!("Missing field features: {name}"))?
        .as_array()
        .ok_or(anyhow!("Malformed field features: {name}"))?
        .iter()
        .map(|feature| feature.as_str().unwrap().to_string())
        .collect();

    Ok((
        name.to_string(),
        CargoOpts {
            features,
            all_features,
            no_default_features,
            git_remote,
            post_hook: None,
        },
    ))
}
