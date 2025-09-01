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
            .map_err(|_| anyhow!("The package list in Arch is not a list"))?
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

        let configured_packages: HashSet<String> =
            groups.lines().flat_map(group_packages).flatten().collect();

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
            .flat_map(|group| {
                if configured.remove(group) {
                    find_group_packages(group, &package_manager).ok()
                } else {
                    None
                }
            })
            .flatten()
            .collect();

        let configured_packages: HashSet<_> = configured_packages
            .into_iter()
            .chain(configured.iter().map(|package| package.to_string()))
            .collect();

        let mut extra = installed.difference(&configured_packages).peekable();

        if extra.peek().is_none() {
            log::info!("No extra packages to remove!");
            Ok(())
        } else {
            log::info!("Removing extra packages");
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
    }
}

fn value_to_pkgspec(value: &Value) -> Result<(String, Option<Closure>)> {
    let record = value.as_record()?;

    let package = record
        .get(PACKAGE_KEY)
        .ok_or(anyhow!("No package mentioned"))?
        .as_str()
        .map_err(|_| anyhow!("The package was not a string"))?
        .to_owned();

    let post_hook = record
        .get(HOOK_KEY)
        .and_then(|closure| {
            closure.as_closure().ok().or_else(|| {
                log::warn!("The closure was not a closure! Ignoring");
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
        .get(ARCH_PACKAGE_MANAGER_KEY)
        .and_then(|paru| {
            paru.as_str().ok().or_else(|| {
                log::warn!("The package manager was not a string! Ignoring");
                None
            })
        })
        .unwrap_or_else(|| {
            log::info!("No package manager mentioned in config. Defaulting to paru");
            DEFAULT_PACKAGE_MANAGER
        })
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
        assert_eq!(res, "paru");
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
        assert_eq!(res, "paru");
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
        assert_eq!(res, "pacman");
    }
}
