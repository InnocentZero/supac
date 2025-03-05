use std::collections::{HashMap, HashSet};

use anyhow::{Result, anyhow, bail};
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
            .map(|value| -> Result<(String, Option<Closure>)> {
                let list = value.as_list()?;
                if list.len() > 2 {
                    bail!("The inner list can only contain two elements: a package and a post-hook closure");
                }

                let post_hook = list.get(1).unwrap().as_closure();

                Ok((
                    list.get(0).unwrap().as_str()?.to_owned(),
                    post_hook
                        .ok()
                        .map(|post_hook| if !post_hook.captures.is_empty() {
                            None
                        } else {
                            Some(post_hook.to_owned())
                        })
                        .unwrap_or(None)
                ))
            })
            .collect::<Result<_>>()?;

        Ok(Arch { packages })
    }

    fn install(&mut self, engine: &mut Engine) -> Result<()> {
        let installed = find_packages()?;

        let mut configured: HashSet<&str> = self.packages.keys().map(String::as_str).collect();

        let groups = run_command_for_stdout(
            // TODO: Change paru to config option
            ["paru", "--sync", "--quiet", "--groups"],
            Perms::User,
            false,
        )?;

        let mut closures = Vec::new();

        let configured_packages: HashSet<String> = groups
            .lines()
            .map(|group| {
                if configured.remove(group) {
                    self.packages
                        .get(group)
                        .unwrap()
                        .as_ref()
                        .map(|closure| closures.push(closure));
                    find_group_packages(group).ok()
                } else {
                    None
                }
            })
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

        if missing.peek().is_none() {
            return Ok(());
        }

        run_command(
            // TODO: Change paru to config option
            ["paru", "--sync"].into_iter().chain(missing),
            Perms::User,
        )?;

        closures
            .iter()
            .map(|closure| engine.execute_closure(closure))
            .collect()
    }
}

fn find_packages() -> Result<HashSet<String>> {
    run_command_for_stdout(
        // TODO: Change paru to config option
        ["paru", "--query", "--explicit", "--quiet"],
        Perms::User,
        false,
    )
    .map(|packages| packages.lines().map(ToOwned::to_owned).collect())
}

fn find_group_packages(group: &str) -> Result<Vec<String>> {
    run_command_for_stdout(
        // TODO: Change paru to config option
        ["paru", "--sync", "--groups", "--quiet", group],
        Perms::User,
        false,
    )
    .map(|packages| packages.lines().map(ToOwned::to_owned).collect())
}
