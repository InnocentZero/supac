use std::collections::{HashMap, HashSet};

use anyhow::{Result, anyhow};
use nu_protocol::Value;
use nu_protocol::{Record, engine::Closure};

use crate::commands::{Perms, run_command, run_command_for_stdout};
use crate::parser::Engine;

use super::Backend;

#[derive(Clone, Debug)]
pub struct FlatpakOpts {
    remote: Option<String>,
    post_hook: Option<Closure>,
}
#[derive(Clone, Debug)]
pub struct PinOpts {
    branch: Option<String>,
    arch: Option<String>,
    post_hook: Option<Closure>,
}

#[derive(Clone, Debug)]
pub struct Flatpak {
    _remotes: HashMap<String, String>,
    pinned: HashMap<String, PinOpts>,
    packages: HashMap<String, FlatpakOpts>,
}

impl Backend for Flatpak {
    fn new(value: &Record) -> Result<Self> {
        let _remotes = value
            .get("remotes")
            .map(|remotes| remotes.as_list().ok())
            .flatten()
            .map(values_to_remotes)
            .unwrap_or_default();

        let pinned = value
            .get("pinned")
            .map(|pinned| pinned.as_list().ok())
            .flatten()
            .map(|values| values.iter().map(value_to_pinspec).flatten().collect())
            .unwrap_or_default();

        let packages = value
            .get("packages")
            .ok_or(anyhow!("Failed to get packages for Flatpak"))?
            .as_list()?
            .iter()
            .map(value_to_pkgspec)
            .collect::<Option<_>>()
            .ok_or(anyhow!("Failed to parse packages for Flatpak"))?;

        log::info!("Successfully parsed flatpak packages");

        Ok(Flatpak {
            _remotes,
            pinned,
            packages,
        })
    }

    fn install(&self, engine: &mut Engine, _: &mut Record) -> Result<()> {
        let mut closures = Vec::new();

        let installed_packages = run_command_for_stdout(
            ["flatpak", "list", "--user", "--columns=application"],
            Perms::User,
            false,
        )?;
        let installed_packages: HashSet<_> = installed_packages.lines().collect();

        self.install_pins(&installed_packages, &mut closures)?;
        self.install_packages(&installed_packages, &mut closures)?;
        log::info!("Successfully installed flatpak packages");

        closures
            .iter()
            .map(|closure| engine.execute_closure(closure))
            .collect::<Result<()>>()?;
        log::info!("Successful flatpak closure execution");

        Ok(())
    }

    fn remove(&self, _: &mut Record) -> Result<()> {
        let pins = run_command_for_stdout(["flatpak", "pin", "--user"], Perms::User, true)?;
        let pins = pins
            .lines()
            .map(|runtime| runtime.trim())
            .map(|runtime| (runtime, parse_runtime_format(runtime)));

        pins.filter(|(_, (runtime, _))| !self.pinned.contains_key(*runtime))
            .map(|(pin, _)| run_command(["flatpak", "pin", "--remove", "--user", pin], Perms::User))
            .collect::<Result<()>>()?;
        log::info!("Removed extra flatpak pins");

        let installed_packages = run_command_for_stdout(
            [
                "flatpak",
                "list",
                "--user",
                "--app",
                "--columns=application",
            ],
            Perms::User,
            false,
        )?;

        let extra_packages = installed_packages
            .lines()
            .filter(|package| !self.packages.contains_key(*package));

        run_command(
            ["flatpak", "remove", "--delete-data"]
                .into_iter()
                .chain(extra_packages),
            Perms::User,
        )?;

        log::info!("Successfully removed extra flatpak packages");

        Ok(())
    }

    fn clean_cache(&self, _config: &Record) -> Result<()> {
        run_command(
            ["flatpak", "remove", "--delete-data", "--unused", "--user"],
            Perms::User,
        )?;
        log::info!("Successfully removed unused flatpak packages");

        Ok(())
    }
}

impl Flatpak {
    fn install_pins<'a>(
        &'a self,
        installed_packages: &HashSet<&str>,
        closures: &mut Vec<&'a Closure>,
    ) -> Result<()> {
        let installed_pins =
            run_command_for_stdout(["flatpak", "pin", "--user"], Perms::User, true)?;

        let installed_pins: HashMap<_, _> = installed_pins
            .lines()
            .map(|runtime| runtime.trim())
            .map(|runtime| parse_runtime_format(runtime))
            .filter(|runtime| installed_packages.contains(runtime.0))
            .collect();

        let configured_pins = &self.pinned;

        let missing_pins: Box<[_]> = configured_pins
            .into_iter()
            .filter(|(package, _)| !installed_pins.contains_key(package.as_str()))
            .map(|(pin, opts)| {
                opts.post_hook.as_ref().map(|hook| closures.push(hook));
                (
                    pin.as_str(),
                    opts.branch.as_ref().map_or("", String::as_str),
                    opts.arch.as_ref().map_or("", String::as_str),
                )
            })
            .collect();

        if !missing_pins.is_empty() {
            missing_pins
                .iter()
                .map(|s| [s.0, s.1, s.2].join("/"))
                .map(|pin| run_command(["flatpak", "pin", "--user", pin.as_str()], Perms::User))
                .collect::<Result<()>>()?;
            log::info!("Pinned the missing runtime patterns");

            run_command(
                ["flatpak", "install", "--user"]
                    .into_iter()
                    .chain(missing_pins.iter().map(|(s, _, _)| *s)),
                Perms::User,
            )?;
            log::info!("Installed the missing runtime patterns");
        }
        Ok(())
    }

    fn install_packages<'a>(
        &'a self,
        installed_packages: &HashSet<&str>,
        closures: &mut Vec<&'a Closure>,
    ) -> Result<()> {
        let free_packages: Box<[_]> = self
            .packages
            .iter()
            .filter(|(_, opts)| opts.remote.is_none())
            .filter(|(package, _)| !installed_packages.contains(package.as_str()))
            .map(|(package, opt)| {
                opt.post_hook.as_ref().map(|hook| closures.push(&hook));
                package.as_str()
            })
            .collect();

        if !free_packages.is_empty() {
            run_command(
                ["flatpak", "install", "--user"]
                    .into_iter()
                    .chain(free_packages.into_iter()),
                Perms::User,
            )?;
        }

        log::info!("Installed remote-agnostic packages");

        let ref_packages = self
            .packages
            .iter()
            .filter(|(package, _)| !installed_packages.contains(package.as_str()))
            .filter_map(|(package, opts)| {
                opts.remote
                    .as_ref()
                    .map(|remote| (package, remote, opts.post_hook.as_ref()))
            });

        for (package, remote, hook) in ref_packages {
            hook.map(|hook| closures.push(&hook));

            run_command(
                ["flatpak", "install", "--user", remote, package],
                Perms::User,
            )?;
        }

        log::info!("Installed remote-specific packages");

        Ok(())
    }
}

fn values_to_remotes(remotes: &[Value]) -> HashMap<String, String> {
    remotes
        .iter()
        .map(|remote| -> Option<_> {
            let remote = remote.as_record().ok();
            let name = remote.map(|record| record.get("package"))??.as_str().ok()?;
            let url = remote.map(|record| record.get("url"))??.as_str().ok()?;
            Some((name.to_owned(), url.to_owned()))
        })
        .flatten()
        .collect()
}

fn value_to_pkgspec(value: &Value) -> Option<(String, FlatpakOpts)> {
    let record = value.as_record().ok()?;
    let name = record.get("package")?.as_str().ok()?.to_owned();

    let remote = record
        .get("remote")
        .map(|remote| remote.as_str().ok())
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
    Some((name, FlatpakOpts { remote, post_hook }))
}

fn value_to_pinspec(value: &Value) -> Option<(String, PinOpts)> {
    let record = value.as_record().ok()?;
    let name = record.get("package")?.as_str().ok()?.to_owned();

    let branch = record
        .get("branch")
        .map(|branch| branch.as_str().ok())
        .flatten()
        .map(ToOwned::to_owned);

    let arch = record
        .get("arch")
        .map(|branch| branch.as_str().ok())
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

    Some((
        name,
        PinOpts {
            branch,
            arch,
            post_hook,
        },
    ))
}

fn parse_runtime_format(runtime: &str) -> (&str, PinOpts) {
    let mut iter = runtime.split('/');
    let runtime = match iter.next() {
        Some("runtime") => iter.next().unwrap(),
        ret => ret.unwrap(),
    };
    let arch = iter.next().map(|s| s.to_owned());
    let branch = iter.next().map(|s| s.to_owned());

    (
        runtime,
        PinOpts {
            arch,
            branch,
            post_hook: None,
        },
    )
}
