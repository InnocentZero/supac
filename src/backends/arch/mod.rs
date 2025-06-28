use std::collections::{HashMap, HashSet};

use anyhow::{Result, anyhow};
use nu_protocol::Value;
use nu_protocol::{Record, engine::Closure};

use crate::commands::{Perms, run_command, run_command_for_stdout};
use crate::parser::Engine;

use super::Backend;

#[derive(Clone, Debug)]
pub struct Arch {
    packages: HashMap<String, Option<Closure>>,
}

impl Backend for Arch {
    fn new(value: &Record) -> Result<Self> {
        let packages = value
            .get("packages")
            .ok_or(anyhow!("Failed to get packages for Arch"))?
            .as_list()?
            .iter()
            .map(value_to_pkgspec)
            .collect::<Result<_>>()?;

        Ok(Arch { packages })
    }

    fn install(&self, engine: &mut Engine, config: &mut Record) -> Result<()> {
        let package_manager = get_package_manager(config);

        let installed = find_packages(package_manager)?;

        let mut configured: HashSet<_> = self.packages.keys().map(String::as_str).collect();

        let groups = run_command_for_stdout(
            [package_manager, "--sync", "--quiet", "--groups"],
            Perms::User,
            false,
        )?;

        let mut closures = Vec::new();

        let group_packages = |group| {
            if configured.remove(group) {
                self.packages
                    .get(group)
                    .unwrap()
                    .as_ref()
                    .map(|closure| closures.push(closure));
                find_group_packages(group, package_manager).ok()
            } else {
                None
            }
        };

        log::info!("Successfully found arch group packages");

        let configured_packages: HashSet<String> = groups
            .lines()
            .map(group_packages)
            .flatten()
            .flatten()
            .collect();

        let missing = &mut configured
            .into_iter()
            .map(|package| {
                self.packages
                    .get(package)
                    .unwrap()
                    .as_ref()
                    .map(|closure| closures.push(&closure));
                package
            })
            .chain(configured_packages.iter().map(String::as_str))
            .filter(|package| !installed.contains(*package))
            .peekable();

        log::info!("Successfully found missing arch packages");

        if missing.peek().is_none() {
            return Ok(());
        }

        log::info!("Successfully checked for no arch missing packages");

        run_command(
            [package_manager, "--sync"].into_iter().chain(missing),
            Perms::User,
        )?;

        log::info!("Successfully installed arch packages");

        closures
            .iter()
            .map(|closure| engine.execute_closure(closure))
            .collect()
    }

    fn remove(&self, config: &mut Record) -> Result<()> {
        let package_manager = get_package_manager(config);

        let installed = find_packages(package_manager)?;

        log::info!("Found installed packages");

        let mut configured: HashSet<_> = self.packages.keys().map(String::as_str).collect();

        let groups = run_command_for_stdout(
            [package_manager, "--sync", "--quiet", "--groups"],
            Perms::User,
            false,
        )?;

        let configured_packages: Box<[_]> = groups
            .lines()
            .map(|group| {
                if configured.remove(group) {
                    find_group_packages(group, &package_manager).ok()
                } else {
                    None
                }
            })
            .flatten()
            .flatten()
            .collect();

        let configured_packages: HashSet<_> = configured_packages
            .into_iter()
            .chain(configured.iter().map(|package| package.to_string()))
            .collect();

        let mut extra = installed.difference(&configured_packages).peekable();

        if extra.peek().is_none() {
            Ok(())
        } else {
            run_command(
                [
                    package_manager,
                    "--remove",
                    "--nosave",
                    "--recursive",
                    "--unneeded",
                ]
                .into_iter()
                .chain(extra.map(String::as_str)),
                Perms::User,
            )
        }
    }

    fn clean_cache(&self, config: &Record) -> Result<()> {
        let package_manager = get_package_manager(config);

        let unused = match run_command_for_stdout(
            [
                package_manager,
                "--query",
                "--deps",
                "--unrequired",
                "--quiet",
            ],
            Perms::User,
            true,
        ) {
            Ok(unused) => unused,
            Err(_) => {
                return Ok(());
            }
        };

        log::info!("Found unused packages");

        let result = run_command(
            [
                package_manager,
                "--remove",
                "--nosave",
                "--recursive",
                "--unneeded",
            ]
            .into_iter()
            .chain(unused.lines()),
            Perms::User,
        );

        log::info!("Removed unused dependencies");

        result
    }
}

fn value_to_pkgspec(value: &Value) -> Result<(String, Option<Closure>)> {
    let record = value.as_record()?;

    let package = record
        .get("package")
        .ok_or(anyhow!("No package mentioned"))?
        .as_str()?
        .to_owned();

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

    Ok((package, post_hook))
}

fn find_packages(package_manager: &str) -> Result<HashSet<String>> {
    run_command_for_stdout(
        [package_manager, "--query", "--explicit", "--quiet"],
        Perms::User,
        false,
    )
    .map(|packages| {
        packages
            .lines()
            .map(str::trim)
            .map(ToOwned::to_owned)
            .collect()
    })
}

fn find_group_packages(group: &str, package_manager: &str) -> Result<Box<[String]>> {
    run_command_for_stdout(
        [package_manager, "--sync", "--groups", "--quiet", group],
        Perms::User,
        false,
    )
    .map(|packages| {
        packages
            .lines()
            .map(str::trim)
            .map(ToOwned::to_owned)
            .collect()
    })
}

fn get_package_manager(config: &Record) -> &str {
    config
        .get("arch_package_manager")
        .map(|paru| -> Option<&str> { paru.as_str().ok() })
        .flatten()
        .unwrap_or("paru")
}
