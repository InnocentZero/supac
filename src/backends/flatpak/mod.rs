use std::collections::{HashMap, HashSet};

use anyhow::{Result, anyhow};
use nu_protocol::Value;
use nu_protocol::{Record, engine::Closure};

use crate::commands::{Perms, dry_run_command, run_command, run_command_for_stdout};
use crate::config::{DEFAULT_FLATPAK_SYSTEMWIDE, FLATPAK_DEFAULT_SYSTEMWIDE_KEY};
use crate::parser::Engine;
use crate::{CleanCommand, SyncCommand, function, mod_err, nest_errors};

use super::Backend;

const REMOTE_LIST_KEY: &str = "remotes";
const PINNED_KEY: &str = "pinned";
const PACKAGE_LIST_KEY: &str = "packages";
const PACKAGE_KEY: &str = "package";
const URL_KEY: &str = "url";
const REMOTE_KEY: &str = "remote";
const HOOK_KEY: &str = "post_hook";
const SYSTEMWIDE_KEY: &str = "systemwide";
const BRANCH_KEY: &str = "branch";
const ARCH_KEY: &str = "arch";

#[derive(Clone, Debug)]
pub struct FlatpakOpts {
    remote: Option<String>,
    systemwide: bool,
    post_hook: Option<Closure>,
}
#[derive(Clone, Debug)]
pub struct PinOpts {
    branch: Option<String>,
    arch: Option<String>,
    systemwide: bool,
    post_hook: Option<Closure>,
}

#[derive(Clone, Debug)]
pub struct Flatpak {
    _remotes: HashMap<String, String>,
    user_pinned: HashMap<String, PinOpts>,
    system_pinned: HashMap<String, PinOpts>,
    user_packages: HashMap<String, FlatpakOpts>,
    system_packages: HashMap<String, FlatpakOpts>,
}

impl Backend for Flatpak {
    fn new(value: &Record, config: &Record) -> Result<Self> {
        let default_systemwide = match config.get(FLATPAK_DEFAULT_SYSTEMWIDE_KEY) {
            Some(val) => val.as_bool().map_err(|e| {
                nest_errors!(
                    "value for {FLATPAK_DEFAULT_SYSTEMWIDE_KEY} not a boolean",
                    e
                )
            })?,
            None => {
                log::info!("Value not specified in config, using default false");
                DEFAULT_FLATPAK_SYSTEMWIDE
            }
        };

        let remotes = match value.get(REMOTE_LIST_KEY) {
            Some(remotes) => remotes
                .as_list()
                .map(values_to_remotes)
                .map_err(|e| nest_errors!("Remotes specified were not a list", e))?,
            None => HashMap::new(),
        };

        let (user_pinned, system_pinned) = match value.get(PINNED_KEY) {
            Some(pinned) => pinned
                .as_list()
                .map(|values| values_to_pins(values, default_systemwide))
                .map_err(|e| nest_errors!("Pinned was not a list", e))?,
            None => (HashMap::new(), HashMap::new()),
        };

        let packages: Box<[_]> = value
            .get(PACKAGE_LIST_KEY)
            .ok_or(mod_err!("Failed to get packages for Flatpak"))?
            .as_list()
            .map_err(|e| nest_errors!("Failed to parse packages for Flatpak", e))?
            .iter()
            .map(|value| value_to_pkgspec(value, default_systemwide))
            .collect::<Result<_>>()?;
        let (user_packages, system_packages) =
            packages.into_iter().partition(|(_, opts)| !opts.systemwide);

        log::info!("Successfully parsed flatpak packages");

        Ok(Flatpak {
            _remotes: remotes,
            user_pinned,
            system_pinned,
            user_packages,
            system_packages,
        })
    }

    fn install(&self, engine: &mut Engine, opts: &SyncCommand) -> Result<()> {
        let mut closures = Vec::new();

        let installed_user_packages = run_command_for_stdout(
            ["flatpak", "list", "--user", "--columns=application"],
            Perms::User,
            false,
        )
        .map_err(|e| nest_errors!("Failed to find listed user flatpak packages", e))?;
        let installed_user_packages: HashSet<_> = installed_user_packages.lines().collect();

        self.install_pins(&installed_user_packages, &mut closures, false, opts)?;
        self.install_packages(&installed_user_packages, &mut closures, false, opts)?;
        log::info!("Successfully installed flatpak packages");

        let installed_system_packages = run_command_for_stdout(
            ["flatpak", "list", "--system", "--columns=application"],
            Perms::User,
            false,
        )
        .map_err(|e| nest_errors!("Failed to find listed user flatpak packages", e))?;
        let installed_system_packages: HashSet<_> = installed_system_packages.lines().collect();

        self.install_pins(&installed_system_packages, &mut closures, true, opts)?;
        self.install_packages(&installed_system_packages, &mut closures, true, opts)?;

        closures
            .iter()
            .try_for_each(|closure| {
                if opts.dry_run {
                    engine.dry_run_closure(closure)
                } else {
                    engine.execute_closure(closure)
                }
            })
            .inspect(|_| log::info!("Successful flatpak closure execution"))
            .map_err(|e| nest_errors!("Failed to execute post hooks", e))
    }

    fn remove(&self, opts: &CleanCommand) -> Result<()> {
        self.remove_pins(false, opts)?;
        self.remove_pins(true, opts)?;

        self.remove_packages(false, opts)?;
        self.remove_packages(true, opts)
    }

    fn clean_cache(&self, _config: &Record) -> Result<()> {
        run_command(
            ["flatpak", "remove", "--delete-data", "--unused", "--user"],
            Perms::User,
        )
        .inspect(|_| log::info!("Successfully removed unused user flatpak packages"))
        .map_err(|e| nest_errors!("Failed to clean cache", e))?;
        run_command(
            ["flatpak", "remove", "--delete-data", "--unused", "--system"],
            Perms::User,
        )
        .inspect(|_| log::info!("Successfully removed unused system flatpak packages"))
        .map_err(|e| nest_errors!("Failed to clean cache", e))
    }
}

impl Flatpak {
    fn install_pins<'a>(
        &'a self,
        installed_packages: &HashSet<&str>,
        closures: &mut Vec<&'a Closure>,
        systemwide: bool,
        command_opts: &SyncCommand,
    ) -> Result<()> {
        let (systemwide_flag, configured_pins) = if systemwide {
            ("--system", &self.system_pinned)
        } else {
            ("--user", &self.user_pinned)
        };

        let installed_pins =
            run_command_for_stdout(["flatpak", "pin", systemwide_flag], Perms::User, true)
                .map_err(|e| nest_errors!("Failed to check for pinned packages", e))?;

        let installed_pins: HashMap<_, _> = installed_pins
            .lines()
            .map(|runtime| runtime.trim())
            .map(|runtime| parse_runtime_format(runtime, false))
            .filter(|runtime| installed_packages.contains(runtime.0))
            .collect();

        let missing_pins: Box<[_]> = configured_pins
            .iter()
            .filter(|(package, _)| !installed_pins.contains_key(package.as_str()))
            .inspect(|(_, opts)| {
                if let Some(hook) = opts.post_hook.as_ref() {
                    closures.push(hook);
                }
            })
            .map(|(pin, opts)| {
                (
                    pin,
                    opts.branch
                        .as_ref()
                        .map(|s| "/".to_owned() + s)
                        .unwrap_or_else(|| "".to_owned()),
                    opts.arch
                        .as_ref()
                        .map(|s| "/".to_owned() + s)
                        .unwrap_or_else(|| "".to_owned()),
                )
            })
            .collect();

        let command_action = if command_opts.dry_run {
            dry_run_command
        } else {
            run_command
        };

        if !missing_pins.is_empty() {
            missing_pins
                .iter()
                .map(|s| [s.0.as_str(), s.1.as_str(), s.2.as_str()].join(""))
                .try_for_each(|pin| {
                    run_command(
                        ["flatpak", "pin", systemwide_flag, pin.as_str()],
                        Perms::User,
                    )
                    .map_err(|e| nest_errors!("Failed to pin packages", e))
                })
                .inspect(|_| log::debug!("Pinned the missing runtime patterns"))?;

            command_action(
                ["flatpak", "install", systemwide_flag]
                    .into_iter()
                    .chain(missing_pins.iter().map(|(s, _, _)| s.as_str())),
                Perms::User,
            )
            .inspect(|_| log::debug!("Installed the missing runtime patterns"))
            .map_err(|e| nest_errors!("Failed to install packages", e))?;
        }

        Ok(())
    }

    fn install_packages<'a>(
        &'a self,
        installed_packages: &HashSet<&str>,
        closures: &mut Vec<&'a Closure>,
        systemwide: bool,
        command_opts: &SyncCommand,
    ) -> Result<()> {
        let (systemwide_flag, configured_packages) = if systemwide {
            ("--system", &self.system_packages)
        } else {
            ("--user", &self.user_packages)
        };

        let mut free_packages = configured_packages
            .iter()
            .filter(|(_, opts)| opts.remote.is_none())
            .filter(|(package, _)| !installed_packages.contains(package.as_str()))
            .inspect(|(_, opt)| {
                if let Some(hook) = opt.post_hook.as_ref() {
                    closures.push(hook);
                }
            })
            .map(|(package, _)| package.as_str())
            .peekable();

        if free_packages.peek().is_some() {
            run_command(
                ["flatpak", "install", systemwide_flag]
                    .into_iter()
                    .chain(free_packages),
                Perms::User,
            )
            .map_err(|e| nest_errors!("failed to install remote-agnostic packages", e))?;
        }

        log::debug!("Installed remote-agnostic packages");

        let ref_packages = configured_packages
            .iter()
            .filter(|(package, _)| !installed_packages.contains(package.as_str()))
            .filter_map(|(package, opts)| {
                opts.remote
                    .as_ref()
                    .map(|remote| (package, remote, opts.post_hook.as_ref()))
            });

        let command_action = if command_opts.dry_run {
            dry_run_command
        } else {
            run_command
        };

        for (package, remote, hook) in ref_packages {
            if let Some(hook) = hook {
                closures.push(hook);
            }

            command_action(
                ["flatpak", "install", systemwide_flag, remote, package],
                Perms::User,
            )
            .map_err(|e| {
                nest_errors!(
                    "Failed to install package {package} from remote {remote}",
                    e
                )
            })?;
        }

        log::debug!("Installed remote-specific packages");

        Ok(())
    }

    fn remove_pins(&self, systemwide: bool, opts: &CleanCommand) -> Result<()> {
        let (systemwide_flag, configured_pins) = if systemwide {
            ("--system", &self.system_pinned)
        } else {
            ("--user", &self.user_pinned)
        };

        let pins = run_command_for_stdout(["flatpak", "pin", systemwide_flag], Perms::User, true)
            .map_err(|e| nest_errors!("Failed to find pinned packages", e))?;

        let pins = pins
            .lines()
            .map(|runtime| runtime.trim())
            .map(|runtime| parse_runtime_format(runtime, false));

        let command_action = if opts.dry_run {
            dry_run_command
        } else {
            run_command
        };

        pins.filter(|(runtime, _)| !configured_pins.contains_key(*runtime))
            .try_for_each(|(pin, _)| {
                command_action(
                    ["flatpak", "pin", "--remove", systemwide_flag, pin],
                    Perms::User,
                )
            })
            .inspect(|_| log::info!("Removed extra flatpak pins"))
            .map_err(|e| nest_errors!("Failed to remove pinned packages", e))
    }

    fn remove_packages(&self, systemwide: bool, opts: &CleanCommand) -> Result<()> {
        let (systemwide_flag, configured_packages) = if systemwide {
            ("--system", &self.system_packages)
        } else {
            ("--user", &self.user_packages)
        };

        let installed_package = run_command_for_stdout(
            [
                "flatpak",
                "list",
                systemwide_flag,
                "--app",
                "--columns=application",
            ],
            Perms::User,
            false,
        )
        .map_err(|e| nest_errors!("Failed to find installed packages", e))?;

        let extra_packages = installed_package
            .lines()
            .filter(|package| !configured_packages.contains_key(*package));

        let command_action = if opts.dry_run {
            dry_run_command
        } else {
            run_command
        };

        command_action(
            ["flatpak", "remove", systemwide_flag, "--delete-data"]
                .into_iter()
                .chain(extra_packages),
            Perms::User,
        )
        .inspect(|_| log::info!("Successfully removed extra flatpak packages"))
        .map_err(|e| nest_errors!("Failed to remove extra packages", e))
    }
}

fn values_to_remotes(remotes: &[Value]) -> HashMap<String, String> {
    remotes.iter().flat_map(extract_remote).collect()
}

fn values_to_pins(
    values: &[Value],
    default_systemwide: bool,
) -> (HashMap<String, PinOpts>, HashMap<String, PinOpts>) {
    values
        .iter()
        .flat_map(|value| value_to_pinspec(value, default_systemwide))
        .partition(|value| !value.1.systemwide)
}

fn value_to_pkgspec(value: &Value, default_systemwide: bool) -> Result<(String, FlatpakOpts)> {
    let record = value
        .as_record()
        .map_err(|e| nest_errors!("pkgspec was not a record", e))?;

    let name = record
        .get(PACKAGE_KEY)
        .ok_or_else(|| mod_err!("record package key is missing"))?
        .as_str()
        .map_err(|e| nest_errors!("record package key is not a string", e))?
        .to_owned();

    let remote = match record.get(REMOTE_KEY) {
        Some(remote) => Some(
            remote
                .as_str()
                .map(ToOwned::to_owned)
                .map_err(|e| nest_errors!("record remote key is not a string in {name}", e))?,
        ),
        None => None,
    };

    let systemwide = record
        .get(SYSTEMWIDE_KEY)
        .map(|val| {
            val.as_bool()
                .map_err(|e| nest_errors!("systemwide for {name} not a boolean", e))
        })
        .unwrap_or_else(|| {
            log::info!("systemwide not specified for {name}, using config default");
            Ok(default_systemwide)
        })?;

    let post_hook = match record.get(HOOK_KEY) {
        Some(post_hook) => {
            let post_hook = post_hook
                .as_closure()
                .map_err(|e| nest_errors!("Post hook for {name} is not a closure", e))?;
            if !post_hook.captures.is_empty() {
                log::warn!("Post hook for {name} captures locals, ignoring");
                None
            } else {
                Some(post_hook.to_owned())
            }
        }
        None => None,
    };

    Ok((
        name,
        FlatpakOpts {
            remote,
            systemwide,
            post_hook,
        },
    ))
}

fn value_to_pinspec(value: &Value, default_systemwide: bool) -> Result<(String, PinOpts)> {
    let record = value
        .as_record()
        .map_err(|e| nest_errors!("pinspec is not a record", e))?;

    let name = record
        .get(PACKAGE_KEY)
        .ok_or_else(|| mod_err!("record package key missing"))?
        .as_str()
        .map_err(|e| nest_errors!("record package key is not a string", e))?
        .to_owned();

    let branch = match record.get(BRANCH_KEY) {
        Some(branch) => Some(
            branch
                .as_str()
                .map(ToOwned::to_owned)
                .map_err(|e| nest_errors!("branch is not a string for {name}", e))?,
        ),
        None => None,
    };

    let arch = match record.get(ARCH_KEY) {
        Some(arch) => Some(
            arch.as_str()
                .map(ToOwned::to_owned)
                .map_err(|e| nest_errors!("arch is not a string for {name}", e))?,
        ),
        None => None,
    };

    let systemwide = record
        .get(SYSTEMWIDE_KEY)
        .map(|val| {
            val.as_bool()
                .map_err(|e| nest_errors!("systemwide for {name} not a boolean", e))
        })
        .unwrap_or_else(|| {
            log::info!("systemwide not specified for {name}, using config default");
            Ok(default_systemwide)
        })?;

    let post_hook = match record.get(HOOK_KEY) {
        Some(closure) => {
            let post_hook = closure
                .as_closure()
                .map_err(|e| nest_errors!("Closure for {name} is not a closure", e))?;

            if !post_hook.captures.is_empty() {
                log::warn!("closure for {name} captures variables, ignoring");
                None
            } else {
                Some(post_hook.to_owned())
            }
        }
        None => None,
    };

    Ok((
        name,
        PinOpts {
            branch,
            arch,
            systemwide,
            post_hook,
        },
    ))
}

fn parse_runtime_format(runtime_string: &str, systemwide: bool) -> (&str, PinOpts) {
    let mut iter = runtime_string.split('/');
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
            systemwide,
            post_hook: None,
        },
    )
}

fn extract_remote(remote: &Value) -> Option<(String, String)> {
    let record = remote.as_record().ok().or_else(|| {
        log::warn!("remote value was not a record, ignoring");
        None
    })?;

    let name = record
        .get(PACKAGE_KEY)
        .or_else(|| {
            log::warn!("remote name was not found, ignoring");
            None
        })?
        .as_str()
        .ok()
        .or_else(|| {
            log::warn!("remote name was not a string, ignoring");
            None
        })?;

    let url = record
        .get(URL_KEY)
        .or_else(|| {
            log::warn!("remote url was not found, ignoring");
            None
        })?
        .as_str()
        .ok()
        .or_else(|| {
            log::warn!("remote url was not a string, ignoring");
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

        let result = value_to_pkgspec(&value, false);
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.0, "org.gtk.Gtk3theme.adw-gtk3");
        assert!(result.1.remote.is_none());
        assert!(!result.1.systemwide);
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

        let result = value_to_pkgspec(&value, false);
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.0, "org.gtk.Gtk3theme.adw-gtk3");
        assert!(result.1.remote.is_none());
        assert!(!result.1.systemwide);
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

        let result = value_to_pkgspec(&value, false);
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.0, "org.gtk.Gtk3theme.adw-gtk3");
        assert!(result.1.remote.is_some());
        let remote = result.1.remote.unwrap();
        assert_eq!(remote, "flathub");
        assert!(!result.1.systemwide);
        assert!(result.1.post_hook.is_some());
    }

    #[test]
    fn value_to_pkgspec_no_systemwide() {
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

        let result = value_to_pkgspec(&value, true);

        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.0, "org.gtk.Gtk3theme.adw-gtk3");
        assert!(result.1.remote.is_none());
        assert!(result.1.systemwide);
        assert!(result.1.post_hook.is_none());
    }

    #[test]
    fn value_to_pkgspec_systemwide() {
        let record = Record::from_raw_cols_vals(
            ["package", "systemwide"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::string("org.gtk.Gtk3theme.adw-gtk3", Span::test_data()),
                Value::bool(true, Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let value = Value::record(record, Span::test_data());

        let result = value_to_pkgspec(&value, false);

        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.0, "org.gtk.Gtk3theme.adw-gtk3");
        assert!(result.1.remote.is_none());
        assert!(result.1.systemwide);
        assert!(result.1.post_hook.is_none());
    }

    #[test]
    fn value_to_pkgspec_systemwide2() {
        let record = Record::from_raw_cols_vals(
            ["package", "systemwide"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::string("org.gtk.Gtk3theme.adw-gtk3", Span::test_data()),
                Value::bool(false, Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let value = Value::record(record, Span::test_data());

        let result = value_to_pkgspec(&value, true);

        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.0, "org.gtk.Gtk3theme.adw-gtk3");
        assert!(result.1.remote.is_none());
        assert!(!result.1.systemwide);
        assert!(result.1.post_hook.is_none());
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

        let result = value_to_pkgspec(&value, false);
        assert!(result.is_err());
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

        let result = value_to_pkgspec(&value, false);
        assert!(result.is_err());
    }

    #[test]
    fn value_to_pkgspec_not_record() {
        let value = Value::bool(false, Span::test_data());

        let result = value_to_pkgspec(&value, false);
        assert!(result.is_err());
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

        let result = value_to_pinspec(&value, false);
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.0, "org.gtk.Gtk3theme.adw-gtk3");
        assert!(result.1.arch.is_none());
        assert!(result.1.branch.is_none());
        assert!(!result.1.systemwide);
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

        let result = value_to_pinspec(&value, false);
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.0, "org.gtk.Gtk3theme.adw-gtk3");
        assert!(result.1.arch.is_none());
        assert!(result.1.branch.is_none());
        assert!(!result.1.systemwide);
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

        let result = value_to_pinspec(&value, false);
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.0, "org.gtk.Gtk3theme.adw-gtk3");
        assert!(result.1.arch.is_some());
        let arch = result.1.arch.unwrap();
        assert_eq!(arch, "x86-64");
        assert!(result.1.branch.is_none());
        assert!(!result.1.systemwide);
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

        let result = value_to_pinspec(&value, false);
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.0, "org.gtk.Gtk3theme.adw-gtk3");
        assert!(result.1.arch.is_some());
        let arch = result.1.arch.unwrap();
        assert_eq!(arch, "x86-64");
        assert!(result.1.branch.is_some());
        let branch = result.1.branch.unwrap();
        assert_eq!(branch, "stable");
        assert!(!result.1.systemwide);
        assert!(result.1.post_hook.is_some());
    }

    #[test]
    fn value_to_pinspec_no_systemwide() {
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

        let result = value_to_pinspec(&value, true);
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.0, "org.gtk.Gtk3theme.adw-gtk3");
        assert!(result.1.arch.is_some());
        let arch = result.1.arch.unwrap();
        assert_eq!(arch, "x86-64");
        assert!(result.1.branch.is_some());
        let branch = result.1.branch.unwrap();
        assert_eq!(branch, "stable");
        assert!(result.1.systemwide);
        assert!(result.1.post_hook.is_some());
    }

    #[test]
    fn value_to_pinspec_systemwide() {
        let closure = Closure {
            block_id: Id::new(0),
            captures: vec![],
        };

        let record = Record::from_raw_cols_vals(
            ["branch", "package", "arch", "systemwide", "post_hook"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::string("stable", Span::test_data()),
                Value::string("org.gtk.Gtk3theme.adw-gtk3", Span::test_data()),
                Value::string("x86-64", Span::test_data()),
                Value::bool(true, Span::test_data()),
                Value::closure(closure, Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let value = Value::record(record, Span::test_data());

        let result = value_to_pinspec(&value, false);
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.0, "org.gtk.Gtk3theme.adw-gtk3");
        assert!(result.1.arch.is_some());
        let arch = result.1.arch.unwrap();
        assert_eq!(arch, "x86-64");
        assert!(result.1.branch.is_some());
        let branch = result.1.branch.unwrap();
        assert_eq!(branch, "stable");
        assert!(result.1.systemwide);
        assert!(result.1.post_hook.is_some());
    }

    #[test]
    fn value_to_pinspec_systemwide2() {
        let closure = Closure {
            block_id: Id::new(0),
            captures: vec![],
        };

        let record = Record::from_raw_cols_vals(
            ["branch", "package", "arch", "systemwide", "post_hook"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::string("stable", Span::test_data()),
                Value::string("org.gtk.Gtk3theme.adw-gtk3", Span::test_data()),
                Value::string("x86-64", Span::test_data()),
                Value::bool(false, Span::test_data()),
                Value::closure(closure, Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let value = Value::record(record, Span::test_data());

        let result = value_to_pinspec(&value, true);
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.0, "org.gtk.Gtk3theme.adw-gtk3");
        assert!(result.1.arch.is_some());
        let arch = result.1.arch.unwrap();
        assert_eq!(arch, "x86-64");
        assert!(result.1.branch.is_some());
        let branch = result.1.branch.unwrap();
        assert_eq!(branch, "stable");
        assert!(!result.1.systemwide);
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

        let result = value_to_pinspec(&value, false);
        assert!(result.is_err());
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

        let result = value_to_pinspec(&value, false);
        assert!(result.is_err());
    }

    #[test]
    fn value_to_pinspec_not_record() {
        let value = Value::bool(false, Span::test_data());

        let result = value_to_pinspec(&value, false);
        assert!(result.is_err());
    }

    #[test]
    fn parse_runtime_format_no_runtime() {
        let runtime = "org.gtk.Gtk3theme.adw-gtk3-dark";

        let res = parse_runtime_format(runtime, false);

        assert!(res.1.branch.is_none());
        assert!(res.1.arch.is_none());
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn parse_runtime_format_no_runtime_arch() {
        let runtime = "org.gtk.Gtk3theme.adw-gtk3-dark/x86-64/";

        let res = parse_runtime_format(runtime, false);

        assert!(res.1.branch.is_none());
        assert!(res.1.arch.is_some());
        assert_eq!(res.1.arch.unwrap(), "x86-64");
        assert!(!res.1.systemwide);
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn parse_runtime_format_no_runtime_branch() {
        let runtime = "runtime/org.gtk.Gtk3theme.adw-gtk3-dark//stable";

        let res = parse_runtime_format(runtime, true);

        assert!(res.1.branch.is_some());
        assert_eq!(res.1.branch.unwrap(), "stable");
        assert!(res.1.arch.is_none());
        assert!(res.1.systemwide);
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn parse_runtime_format_no_runtime_arch_branch() {
        let runtime = "org.gtk.Gtk3theme.adw-gtk3-dark/x86-64/stable";

        let res = parse_runtime_format(runtime, false);

        assert!(res.1.branch.is_some());
        assert_eq!(res.1.branch.unwrap(), "stable");
        assert!(res.1.arch.is_some());
        assert_eq!(res.1.arch.unwrap(), "x86-64");
        assert!(!res.1.systemwide);
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn parse_runtime_format_runtime() {
        let runtime = "runtime/org.gtk.Gtk3theme.adw-gtk3-dark";

        let res = parse_runtime_format(runtime, true);

        assert!(res.1.branch.is_none());
        assert!(res.1.arch.is_none());
        assert!(res.1.systemwide);
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn parse_runtime_format_arch() {
        let runtime = "runtime/org.gtk.Gtk3theme.adw-gtk3-dark/x86-64";

        let res = parse_runtime_format(runtime, false);

        assert!(res.1.branch.is_none());
        assert!(res.1.arch.is_some());
        assert_eq!(res.1.arch.unwrap(), "x86-64");
        assert!(!res.1.systemwide);
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn parse_runtime_format_branch() {
        let runtime = "runtime/org.gtk.Gtk3theme.adw-gtk3-dark//stable";

        let res = parse_runtime_format(runtime, true);

        assert!(res.1.branch.is_some());
        assert_eq!(res.1.branch.unwrap(), "stable");
        assert!(res.1.arch.is_none());
        assert!(res.1.systemwide);
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn parse_runtime_format_arch_branch() {
        let runtime = "runtime/org.gtk.Gtk3theme.adw-gtk3-dark/x86-64/stable";

        let res = parse_runtime_format(runtime, false);

        assert!(res.1.branch.is_some());
        assert_eq!(res.1.branch.unwrap(), "stable");
        assert!(res.1.arch.is_some());
        assert_eq!(res.1.arch.unwrap(), "x86-64");
        assert!(!res.1.systemwide);
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
