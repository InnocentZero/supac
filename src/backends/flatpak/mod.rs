use std::collections::{HashMap, HashSet};

use anyhow::{Result, anyhow};
use nu_protocol::Value;
use nu_protocol::{Record, engine::Closure};

use crate::commands::{Perms, run_command, run_command_for_stdout};
use crate::parser::Engine;

use super::Backend;

const REMOTE_LIST_KEY: &str = "remotes";
const PINNED_KEY: &str = "pinned";
const PACKAGE_LIST_KEY: &str = "packages";
const PACKAGE_KEY: &str = "package";
const URL_KEY: &str = "url";
const REMOTE_KEY: &str = "remote";
const HOOK_KEY: &str = "post_hook";
const BRANCH_KEY: &str = "branch";
const ARCH_KEY: &str = "arch";

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
            .get(REMOTE_LIST_KEY)
            .map(|remotes| {
                remotes.as_list().ok().or_else(|| {
                    log::warn!("remotes {remotes:#?} was not a list, ignoring");
                    None
                })
            })
            .flatten()
            .map(values_to_remotes)
            .unwrap_or_default();

        let pinned = value
            .get(PINNED_KEY)
            .map(|pinned| {
                pinned.as_list().ok().or_else(|| {
                    log::warn!("Pinned {pinned:#?} was not a list, ignoring");
                    None
                })
            })
            .flatten()
            .map(|values| values.iter().map(value_to_pinspec).flatten().collect())
            .unwrap_or_default();

        let packages = value
            .get(PACKAGE_LIST_KEY)
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
            .map(|runtime| parse_runtime_format(runtime));

        pins.filter(|(runtime, _)| !self.pinned.contains_key(*runtime))
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
    remotes.iter().flat_map(extract_remote).collect()
}

fn value_to_pkgspec(value: &Value) -> Option<(String, FlatpakOpts)> {
    let record = value.as_record().ok().or_else(|| {
        log::warn!("pkgspec {value:#?} was not a record, ignoring");
        None
    })?;

    let name = record
        .get(PACKAGE_KEY)
        .or_else(|| {
            log::warn!("record {record:#?} package key is missing, ignoring");
            None
        })?
        .as_str()
        .ok()
        .or_else(|| {
            log::warn!("record {record:#?} package key is not a string, ignoring");
            None
        })?
        .to_owned();

    let remote = record
        .get(REMOTE_KEY)
        .map(|remote| {
            remote.as_str().ok().or_else(|| {
                log::warn!("record {record:#?} remote key is not a string, ignoring");
                None
            })
        })
        .flatten()
        .map(ToOwned::to_owned);

    let post_hook = record
        .get(HOOK_KEY)
        .map(|closure| {
            closure.as_closure().ok().or_else(|| {
                log::warn!("record {record:#?} hook is not a closure, ignoring");
                None
            })
        })
        .flatten()
        .map(|post_hook| {
            if !post_hook.captures.is_empty() {
                log::warn!("closure {post_hook:#?} captures locals, ignoring");
                None
            } else {
                Some(post_hook.to_owned())
            }
        })
        .flatten();

    Some((name, FlatpakOpts { remote, post_hook }))
}

fn value_to_pinspec(value: &Value) -> Option<(String, PinOpts)> {
    let record = value.as_record().ok().or_else(|| {
        log::warn!("pinspec {value:#?} is not a record, ignoring");
        None
    })?;

    let name = record
        .get(PACKAGE_KEY)
        .or_else(|| {
            log::warn!("record {record:#?} package key missing, ignoring");
            None
        })?
        .as_str()
        .ok()
        .or_else(|| {
            log::warn!("record {record:#?} package key is not a string, ignoring");
            None
        })?
        .to_owned();

    let branch = record
        .get(BRANCH_KEY)
        .map(|branch| {
            branch.as_str().ok().or_else(|| {
                log::warn!("record {record:#?} branch is not a string, ignoring");
                None
            })
        })
        .flatten()
        .map(ToOwned::to_owned);

    let arch = record
        .get(ARCH_KEY)
        .map(|arch| {
            arch.as_str().ok().or_else(|| {
                log::warn!("record {record:#?} arch is not a string, ignoring");
                None
            })
        })
        .flatten()
        .map(ToOwned::to_owned);

    let post_hook = record
        .get(HOOK_KEY)
        .map(|closure| {
            closure.as_closure().ok().or_else(|| {
                log::warn!("record {record:#?} hook is not a closure, ignoring");
                None
            })
        })
        .flatten()
        .map(|post_hook| {
            if !post_hook.captures.is_empty() {
                log::warn!("closure {post_hook:#?} captures variables, ignoring");
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
    let arch = iter.next().filter(|s| !s.is_empty()).map(|s| s.to_owned());
    let branch = iter.next().filter(|s| !s.is_empty()).map(|s| s.to_owned());

    (
        runtime,
        PinOpts {
            arch,
            branch,
            post_hook: None,
        },
    )
}

fn extract_remote(remote: &Value) -> Option<(String, String)> {
    let record = remote.as_record().ok().or_else(|| {
        log::warn!("remote value {remote:#?} was not a record, ignoring");
        None
    })?;

    let name = record
        .get(PACKAGE_KEY)
        .or_else(|| {
            log::warn!("remote {remote:#?} name  was not found, ignoring");
            None
        })?
        .as_str()
        .ok()
        .or_else(|| {
            log::warn!("remote {remote:#?} name was not a string, ignoring");
            None
        })?;

    let url = record
        .get(URL_KEY)
        .or_else(|| {
            log::warn!("remote {remote:#?} url  was not found, ignoring");
            None
        })?
        .as_str()
        .ok()
        .or_else(|| {
            log::warn!("remote {remote:#?} url was not a string, ignoring");
            None
        })?;

    Some((name.to_owned(), url.to_owned()))
}

#[cfg(test)]
mod test {
    use nu_protocol::{Id, Span};

    use super::*;

    #[test]
    fn value_to_pkgspec_no_opts() {
        let record = Record::from_raw_cols_vals(
            ["package"].into_iter().map(ToOwned::to_owned).collect(),
            vec![Value::string(
                "org.gtk.Gtk3theme.adw-gtk3",
                Span::test_data(),
            )],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let value = Value::record(record, Span::test_data());

        let result = value_to_pkgspec(&value);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.0, "org.gtk.Gtk3theme.adw-gtk3");
        assert!(result.1.remote.is_none());
        assert!(result.1.post_hook.is_none());
    }

    #[test]
    fn value_to_pkgspec_hook() {
        let closure = Closure {
            block_id: Id::new(0),
            captures: vec![],
        };

        let record = Record::from_raw_cols_vals(
            ["package", "post_hook"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::string("org.gtk.Gtk3theme.adw-gtk3", Span::test_data()),
                Value::closure(closure, Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let value = Value::record(record, Span::test_data());

        let result = value_to_pkgspec(&value);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.0, "org.gtk.Gtk3theme.adw-gtk3");
        assert!(result.1.remote.is_none());
        assert!(result.1.post_hook.is_some());
    }

    #[test]
    fn value_to_pkgspec_arch() {
        let closure = Closure {
            block_id: Id::new(0),
            captures: vec![],
        };

        let record = Record::from_raw_cols_vals(
            ["package", "remote", "post_hook"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::string("org.gtk.Gtk3theme.adw-gtk3", Span::test_data()),
                Value::string("flathub", Span::test_data()),
                Value::closure(closure, Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let value = Value::record(record, Span::test_data());

        let result = value_to_pkgspec(&value);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.0, "org.gtk.Gtk3theme.adw-gtk3");
        assert!(result.1.remote.is_some());
        let remote = result.1.remote.unwrap();
        assert_eq!(remote, "flathub");
        assert!(result.1.post_hook.is_some());
    }

    #[test]
    fn value_to_pkgspec_wrong() {
        let closure = Closure {
            block_id: Id::new(0),
            captures: vec![],
        };

        let record = Record::from_raw_cols_vals(
            ["package", "remote", "post_hook"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::bool(false, Span::test_data()),
                Value::string("foo", Span::test_data()),
                Value::closure(closure, Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let value = Value::record(record, Span::test_data());

        let result = value_to_pkgspec(&value);
        assert!(result.is_none());
    }

    #[test]
    fn value_to_pkgspec_wrong_fields() {
        let closure = Closure {
            block_id: Id::new(0),
            captures: vec![],
        };

        let record = Record::from_raw_cols_vals(
            ["package", "remote", "post_hook"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::string("org.gtk.Gtk3theme.adw-gtk3", Span::test_data()),
                Value::closure(closure, Span::test_data()),
                Value::string("foo", Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let value = Value::record(record, Span::test_data());

        let result = value_to_pkgspec(&value);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.0, "org.gtk.Gtk3theme.adw-gtk3");
        assert!(result.1.remote.is_none());
        assert!(result.1.post_hook.is_none());
    }

    #[test]
    fn value_to_pkgspec_not_record() {
        let value = Value::bool(false, Span::test_data());

        let result = value_to_pkgspec(&value);
        assert!(result.is_none());
    }

    #[test]
    fn value_to_pinspec_no_opts() {
        let record = Record::from_raw_cols_vals(
            ["package"].into_iter().map(ToOwned::to_owned).collect(),
            vec![Value::string(
                "org.gtk.Gtk3theme.adw-gtk3",
                Span::test_data(),
            )],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let value = Value::record(record, Span::test_data());

        let result = value_to_pinspec(&value);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.0, "org.gtk.Gtk3theme.adw-gtk3");
        assert!(result.1.arch.is_none());
        assert!(result.1.branch.is_none());
        assert!(result.1.post_hook.is_none());
    }

    #[test]
    fn value_to_pinspec_hook() {
        let closure = Closure {
            block_id: Id::new(0),
            captures: vec![],
        };

        let record = Record::from_raw_cols_vals(
            ["package", "post_hook"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::string("org.gtk.Gtk3theme.adw-gtk3", Span::test_data()),
                Value::closure(closure, Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let value = Value::record(record, Span::test_data());

        let result = value_to_pinspec(&value);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.0, "org.gtk.Gtk3theme.adw-gtk3");
        assert!(result.1.arch.is_none());
        assert!(result.1.branch.is_none());
        assert!(result.1.post_hook.is_some());
    }

    #[test]
    fn value_to_pinspec_arch() {
        let closure = Closure {
            block_id: Id::new(0),
            captures: vec![],
        };

        let record = Record::from_raw_cols_vals(
            ["package", "arch", "post_hook"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::string("org.gtk.Gtk3theme.adw-gtk3", Span::test_data()),
                Value::string("x86-64", Span::test_data()),
                Value::closure(closure, Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let value = Value::record(record, Span::test_data());

        let result = value_to_pinspec(&value);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.0, "org.gtk.Gtk3theme.adw-gtk3");
        assert!(result.1.arch.is_some());
        let arch = result.1.arch.unwrap();
        assert_eq!(arch, "x86-64");
        assert!(result.1.branch.is_none());
        assert!(result.1.post_hook.is_some());
    }

    #[test]
    fn value_to_pinspec_branch() {
        let closure = Closure {
            block_id: Id::new(0),
            captures: vec![],
        };

        let record = Record::from_raw_cols_vals(
            ["branch", "package", "arch", "post_hook"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::string("stable", Span::test_data()),
                Value::string("org.gtk.Gtk3theme.adw-gtk3", Span::test_data()),
                Value::string("x86-64", Span::test_data()),
                Value::closure(closure, Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let value = Value::record(record, Span::test_data());

        let result = value_to_pinspec(&value);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.0, "org.gtk.Gtk3theme.adw-gtk3");
        assert!(result.1.arch.is_some());
        let arch = result.1.arch.unwrap();
        assert_eq!(arch, "x86-64");
        assert!(result.1.branch.is_some());
        let branch = result.1.branch.unwrap();
        assert_eq!(branch, "stable");
        assert!(result.1.post_hook.is_some());
    }

    #[test]
    fn value_to_pinspec_wrong() {
        let closure = Closure {
            block_id: Id::new(0),
            captures: vec![],
        };

        let record = Record::from_raw_cols_vals(
            ["branch", "package", "arch", "post_hook"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::string("stable", Span::test_data()),
                Value::bool(false, Span::test_data()),
                Value::string("x86-64", Span::test_data()),
                Value::closure(closure, Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let value = Value::record(record, Span::test_data());

        let result = value_to_pinspec(&value);
        assert!(result.is_none());
    }

    #[test]
    fn value_to_pinspec_wrong_fields() {
        let closure = Closure {
            block_id: Id::new(0),
            captures: vec![],
        };

        let record = Record::from_raw_cols_vals(
            ["branch", "package", "arch", "post_hook"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::bool(false, Span::test_data()),
                Value::string("org.gtk.Gtk3theme.adw-gtk3", Span::test_data()),
                Value::closure(closure, Span::test_data()),
                Value::string("foo", Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let value = Value::record(record, Span::test_data());

        let result = value_to_pinspec(&value);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.0, "org.gtk.Gtk3theme.adw-gtk3");
        assert!(result.1.arch.is_none());
        assert!(result.1.branch.is_none());
        assert!(result.1.post_hook.is_none());
    }

    #[test]
    fn value_to_pinspec_not_record() {
        let value = Value::bool(false, Span::test_data());

        let result = value_to_pinspec(&value);
        assert!(result.is_none());
    }

    #[test]
    fn parse_runtime_format_no_runtime() {
        let runtime = "org.gtk.Gtk3theme.adw-gtk3-dark";

        let res = parse_runtime_format(runtime);

        assert!(res.1.branch.is_none());
        assert!(res.1.arch.is_none());
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn parse_runtime_format_no_runtime_arch() {
        let runtime = "org.gtk.Gtk3theme.adw-gtk3-dark/x86-64/";

        let res = parse_runtime_format(runtime);

        assert!(res.1.branch.is_none());
        assert!(res.1.arch.is_some());
        assert_eq!(res.1.arch.unwrap(), "x86-64");
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn parse_runtime_format_no_runtime_branch() {
        let runtime = "runtime/org.gtk.Gtk3theme.adw-gtk3-dark//stable";

        let res = parse_runtime_format(runtime);

        assert!(res.1.branch.is_some());
        assert_eq!(res.1.branch.unwrap(), "stable");
        assert!(res.1.arch.is_none());
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn parse_runtime_format_no_runtime_arch_branch() {
        let runtime = "org.gtk.Gtk3theme.adw-gtk3-dark/x86-64/stable";

        let res = parse_runtime_format(runtime);

        assert!(res.1.branch.is_some());
        assert_eq!(res.1.branch.unwrap(), "stable");
        assert!(res.1.arch.is_some());
        assert_eq!(res.1.arch.unwrap(), "x86-64");
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn parse_runtime_format_runtime() {
        let runtime = "runtime/org.gtk.Gtk3theme.adw-gtk3-dark";

        let res = parse_runtime_format(runtime);

        assert!(res.1.branch.is_none());
        assert!(res.1.arch.is_none());
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn parse_runtime_format_arch() {
        let runtime = "runtime/org.gtk.Gtk3theme.adw-gtk3-dark/x86-64";

        let res = parse_runtime_format(runtime);

        assert!(res.1.branch.is_none());
        assert!(res.1.arch.is_some());
        assert_eq!(res.1.arch.unwrap(), "x86-64");
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn parse_runtime_format_branch() {
        let runtime = "runtime/org.gtk.Gtk3theme.adw-gtk3-dark//stable";

        let res = parse_runtime_format(runtime);

        assert!(res.1.branch.is_some());
        assert_eq!(res.1.branch.unwrap(), "stable");
        assert!(res.1.arch.is_none());
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn parse_runtime_format_arch_branch() {
        let runtime = "runtime/org.gtk.Gtk3theme.adw-gtk3-dark/x86-64/stable";

        let res = parse_runtime_format(runtime);

        assert!(res.1.branch.is_some());
        assert_eq!(res.1.branch.unwrap(), "stable");
        assert!(res.1.arch.is_some());
        assert_eq!(res.1.arch.unwrap(), "x86-64");
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn value_to_remote_ok() {
        let value = Record::from_raw_cols_vals(
            ["package", "url"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            ["a", "b"]
                .into_iter()
                .map(|a| Value::string(a, Span::test_data()))
                .collect(),
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();
        let value = Value::record(value, Span::test_data());

        let res = extract_remote(&value);
        let check = Some(("a".to_owned(), "b".to_owned()));

        assert_eq!(check, res);
    }

    #[test]
    fn value_to_remote_not_records() {
        let value = Value::string("a", Span::test_data());
        let res = extract_remote(&value);
        let check = None;
        assert_eq!(check, res);
    }

    #[test]
    fn values_to_remote_not_package() {
        let value = Record::from_raw_cols_vals(
            ["pkg", "url"].into_iter().map(ToOwned::to_owned).collect(),
            ["a", "b"]
                .into_iter()
                .map(|a| Value::string(a, Span::test_data()))
                .collect(),
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();
        let value = Value::record(value, Span::test_data());

        let res = extract_remote(&value);
        let check = None;
        assert_eq!(check, res);
    }
}
