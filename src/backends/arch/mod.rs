use std::collections::{HashMap, HashSet};

use anyhow::{Result, anyhow};
use nu_protocol::Value;
use nu_protocol::{Record, engine::Closure};

use crate::commands::{Perms, run_command, run_command_for_stdout};
use crate::parser::Engine;

use super::Backend;

const PACKAGE_LIST_KEY: &str = "packages";
const PACKAGE_KEY: &str = "package";
const HOOK_KEY: &str = "post_hook";

const DEFAULT_PACKAGE_MANAGER: &str = "paru";
const ARCH_PACKAGE_MANAGER_KEY: &str = "arch_package_manager";

#[derive(Clone, Debug)]
pub struct Arch {
    packages: HashMap<String, Option<Closure>>,
}

impl Backend for Arch {
    fn new(value: &Record) -> Result<Self> {
        let packages = value
            .get(PACKAGE_LIST_KEY)
            .ok_or(anyhow!("Failed to get packages for Arch"))?
            .as_list()
            .map_err(|e| anyhow!("The package list in Arch is not a list\n {e}"))?
            .iter()
            .map(value_to_pkgspec)
            .collect::<Result<_>>()?;

        log::info!("Successfully parsed arch packages");
        Ok(Arch { packages })
    }

    fn install(&self, engine: &mut Engine, config: &mut Record) -> Result<()> {
        let (package_manager, perms) = get_package_manager(config)?;

        let installed = get_installed_packages(package_manager)?;

        let mut configured: HashSet<_> = self.packages.keys().map(String::as_str).collect();

        let groups = run_command_for_stdout(
            [package_manager, "--sync", "--quiet", "--groups"],
            perms,
            false,
        )
        .map_err(|e| anyhow!("Failed to get group packages\n {e}"))?;

        let mut closures = Vec::new();

        let configured_group_packages: Box<_> = groups
            .lines()
            .filter(|group| configured.remove(group))
            .inspect(|group| {
                if let Some(closure) = self.packages.get(*group).unwrap().as_ref() {
                    closures.push(closure);
                }
            })
            .map(|group| get_installed_group_packages(group, package_manager))
            .collect::<Result<_>>()?;

        let missing = &mut configured
            .into_iter()
            .inspect(|package| {
                if let Some(closure) = self.packages.get(*package).unwrap().as_ref() {
                    closures.push(closure);
                }
            })
            .chain(
                configured_group_packages
                    .iter()
                    .flatten()
                    .map(String::as_str),
            )
            .filter(|package| !installed.contains(*package))
            .peekable();

        log::info!("Successfully found all missing arch packages");

        if missing.peek().is_none() {
            log::info!("Nothing to install!");
            return Ok(());
        }

        run_command(
            [package_manager, "--sync"].into_iter().chain(missing),
            Perms::User,
        )
        .inspect(|_| log::info!("Successfully installed arch packages"))
        .map_err(|e| anyhow!("Failed to install packages\n {e}"))?;

        closures
            .iter()
            .try_for_each(|closure| engine.execute_closure(closure))
            .inspect(|_| log::info!("Successfully executed all closures"))
            .map_err(|e| anyhow!("Failed to execute closures\n {e}"))
    }

    fn remove(&self, config: &mut Record) -> Result<()> {
        let (package_manager, perms) = get_package_manager(config)?;

        let installed = get_installed_packages(package_manager)?;

        let mut configured: HashSet<_> = self.packages.keys().map(String::as_str).collect();

        let groups = run_command_for_stdout(
            [package_manager, "--sync", "--quiet", "--groups"],
            perms,
            false,
        )?;

        let configured_packages: Box<[_]> = groups
            .lines()
            .filter(|group| configured.remove(group))
            .map(|group| get_installed_group_packages(group, package_manager))
            .collect::<Result<_>>()?;

        let configured_packages: HashSet<_> = configured_packages
            .into_iter()
            .flatten()
            .chain(configured.iter().map(|package| package.to_string()))
            .collect();

        let mut extra = installed.difference(&configured_packages).peekable();

        if extra.peek().is_none() {
            log::info!("No extra packages to remove!");
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
            .inspect(|_| log::info!("Removed extra packages"))
            .map_err(|e| anyhow!("Failed to remove packages\n {e}"))
        }
    }

    fn clean_cache(&self, config: &Record) -> Result<()> {
        let (package_manager, perms) = get_package_manager(config)?;

        let unused = run_command_for_stdout(
            [
                package_manager,
                "--query",
                "--deps",
                "--unrequired",
                "--quiet",
            ],
            perms,
            true,
        );

        // arch package managers fail when there are no packages
        let unused = match unused {
            Ok(unused) => unused,
            Err(_) => {
                log::info!("No unused dependencies to remove");
                return Ok(());
            }
        };

        log::info!("Found unused packages, Removing unused dependencies");

        run_command(
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
        )
        .inspect(|_| log::info!("Successfully removed all unused dependencies"))
        .map_err(|e| anyhow!("Failed to clean cache for arch\n {e}"))
    }
}

fn value_to_pkgspec(value: &Value) -> Result<(String, Option<Closure>)> {
    let record = value
        .as_record()
        .map_err(|e| anyhow!("The package-spec is not a record\n {e}"))?;

    let package = record
        .get(PACKAGE_KEY)
        .ok_or(anyhow!("No package mentioned"))?
        .as_str()
        .map_err(|e| anyhow!("The package was not a string\n {e}"))?
        .to_owned();

    let post_hook = record
        .get(HOOK_KEY)
        .and_then(|closure| {
            closure.as_closure().ok().or_else(|| {
                log::warn!("Closure supplied in {package} was not a closure! Ignoring");
                None
            })
        })
        .and_then(|post_hook| {
            if !post_hook.captures.is_empty() {
                log::warn!("Closure was trying to access a local variable");
                None
            } else {
                Some(post_hook.to_owned())
            }
        });

    Ok((package, post_hook))
}

fn get_installed_packages(package_manager: &str) -> Result<HashSet<String>> {
    let packages = run_command_for_stdout(
        [package_manager, "--query", "--explicit", "--quiet"],
        Perms::User,
        false,
    )
    .map_err(|e| anyhow!("Failed to get installed packages for {package_manager}\n {e}"))?;

    let packages = packages
        .lines()
        .map(str::trim)
        .map(ToOwned::to_owned)
        .collect();

    Ok(packages)
}

fn get_installed_group_packages(group: &str, package_manager: &str) -> Result<Box<[String]>> {
    let packages = run_command_for_stdout(
        [package_manager, "--sync", "--groups", "--quiet", group],
        Perms::User,
        false,
    )
    .map_err(|e| anyhow!("failed to get package groups with {package_manager}\n {e}"))?;

    let packages = packages
        .lines()
        .map(str::trim)
        .map(ToOwned::to_owned)
        .collect();

    Ok(packages)
}

fn get_package_manager(config: &Record) -> Result<(&str, Perms)> {
    let pacman = match config.get(ARCH_PACKAGE_MANAGER_KEY) {
        Some(pacman) => pacman.as_str().map_err(|e| {
            anyhow!("Failed to parse config, arch package manager is not a string\n {e}")
        })?,
        None => DEFAULT_PACKAGE_MANAGER,
    };

    if pacman == "pacman" {
        Ok((pacman, Perms::Root))
    } else {
        Ok((pacman, Perms::User))
    }
}

#[cfg(test)]
mod test {
    use nu_protocol::{Id, Span};

    use super::*;

    #[test]
    fn arch_construction_ok() {
        let pkg_record = Record::from_raw_cols_vals(
            vec!["package".to_owned()],
            vec![Value::string("aerospace", Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();
        let package_list = Value::list(
            vec![Value::record(pkg_record, Span::test_data())],
            Span::test_data(),
        );

        let record = Record::from_raw_cols_vals(
            ["packages", "foo"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![package_list, Value::nothing(Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let arch = Arch::new(&record);
        assert!(arch.is_ok());
        let arch = arch.unwrap();
        assert_eq!(arch.packages.len(), 1);
        assert!(
            arch.packages
                .keys()
                .collect::<HashSet<_>>()
                .contains(&"aerospace".to_owned())
        );
    }

    #[test]
    fn arch_construction_not_list() {
        let pkg_record = Record::from_raw_cols_vals(
            vec!["package".to_owned()],
            vec![Value::string("aerospace", Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let package_list = Value::record(pkg_record, Span::test_data());

        let record = Record::from_raw_cols_vals(
            ["packages", "foo"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![package_list, Value::nothing(Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let arch = Arch::new(&record);
        assert!(arch.is_err());
    }

    #[test]
    fn arch_construction_partially_improper() {
        let pkg_record = Record::from_raw_cols_vals(
            vec!["package".to_owned()],
            vec![Value::string("aerospace", Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let pkg_record_2 = Record::from_raw_cols_vals(
            vec!["pkg".to_owned()],
            vec![Value::string("aerospace", Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let package_list = vec![
            Value::record(pkg_record, Span::test_data()),
            Value::record(pkg_record_2, Span::test_data()),
        ];

        let record = Record::from_raw_cols_vals(
            ["packages", "foo"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::list(package_list, Span::test_data()),
                Value::nothing(Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let arch = Arch::new(&record);
        assert!(arch.is_err());
    }

    #[test]
    fn arch_construction_no_packages() {
        let pkg_record = Record::from_raw_cols_vals(
            vec!["package".to_owned()],
            vec![Value::string("aerospace", Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let pkg_record_2 = Record::from_raw_cols_vals(
            vec!["pkg".to_owned()],
            vec![Value::string("aerospace", Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let package_list = vec![
            Value::record(pkg_record, Span::test_data()),
            Value::record(pkg_record_2, Span::test_data()),
        ];

        let record = Record::from_raw_cols_vals(
            ["pkgs", "foo"].into_iter().map(ToOwned::to_owned).collect(),
            vec![
                Value::list(package_list, Span::test_data()),
                Value::nothing(Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let arch = Arch::new(&record);
        assert!(arch.is_err());
    }

    #[test]
    fn val_to_pkgspec_regular() {
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
                Value::string("foo", Span::test_data()),
                Value::closure(closure.clone(), Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let value = Value::record(record, Span::test_data());

        let res = value_to_pkgspec(&value);

        assert!(res.is_ok());

        let (package, closure_opt) = res.unwrap();
        assert_eq!(package, "foo");

        assert!(closure_opt.is_some());
        assert_eq!(closure_opt.as_ref().unwrap().block_id, closure.block_id);
        assert_eq!(closure_opt.unwrap().captures, vec![]);
    }

    #[test]
    fn val_to_pkgspec_bound_closure() {
        let closure = Closure {
            block_id: Id::new(0),
            captures: vec![(Id::new(1), Value::bool(true, Span::test_data()))],
        };
        let record = Record::from_raw_cols_vals(
            ["package", "post_hook"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::string("foo", Span::test_data()),
                Value::closure(closure.clone(), Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let value = Value::record(record, Span::test_data());

        let res = value_to_pkgspec(&value);

        assert!(res.is_ok());

        let (package, closure_opt) = res.unwrap();
        assert_eq!(package, "foo");

        assert!(closure_opt.is_none());
    }

    #[test]
    fn val_to_pkgspec_no_closure() {
        let record = Record::from_raw_cols_vals(
            vec!["package".to_owned()],
            vec![Value::string("foo", Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let value = Value::record(record, Span::test_data());

        let res = value_to_pkgspec(&value);
        assert!(res.is_ok());

        let (package, closure_opt) = res.unwrap();
        assert_eq!(package, "foo");

        assert!(closure_opt.is_none());
    }

    #[test]
    fn val_to_pkgspec_random_field() {
        let record = Record::from_raw_cols_vals(
            ["package", "random_field"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::string("foo", Span::test_data()),
                Value::string("bar", Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let value = Value::record(record, Span::test_data());

        let res = value_to_pkgspec(&value);
        assert!(res.is_ok());

        let (package, closure_opt) = res.unwrap();
        assert_eq!(package, "foo");

        assert!(closure_opt.is_none());
    }

    #[test]
    fn val_to_pkgspec_missing_package() {
        let record = Record::from_raw_cols_vals(
            ["not_package", "random_field"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::string("foo", Span::test_data()),
                Value::string("bar", Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let value = Value::record(record, Span::test_data());

        let res = value_to_pkgspec(&value);
        assert!(res.is_err());
    }

    #[test]
    fn pkgmgr_absent() {
        let config =
            Record::from_raw_cols_vals(vec![], vec![], Span::test_data(), Span::test_data())
                .unwrap();
        let res = get_package_manager(&config);
        assert!(res.is_ok());
        let (pm, perms) = res.unwrap();
        assert_eq!(pm, "paru");
        assert_eq!(perms, Perms::User);
    }

    #[test]
    fn pkgmgr_others() {
        let config = Record::from_raw_cols_vals(
            vec!["foo".to_owned()],
            vec![Value::string("bar", Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();
        let res = get_package_manager(&config);
        assert!(res.is_ok());
        let (pm, perms) = res.unwrap();
        assert_eq!(pm, "paru");
        assert_eq!(perms, Perms::User);
    }

    #[test]
    fn pkgmgr_present() {
        let config = Record::from_raw_cols_vals(
            vec!["arch_package_manager".to_owned()],
            vec![Value::string("pacman", Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();
        let res = get_package_manager(&config);
        assert!(res.is_ok());
        let (pm, perms) = res.unwrap();
        assert_eq!(pm, "pacman");
        assert_eq!(perms, Perms::Root);
    }

    #[test]
    fn pkgmgr_wrong() {
        let config = Record::from_raw_cols_vals(
            vec!["arch_package_manager".to_owned()],
            vec![Value::bool(true, Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();
        let res = get_package_manager(&config);
        assert!(res.is_err());
    }
}
