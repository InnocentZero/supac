use std::collections::{HashMap, HashSet};
use std::fs;

use anyhow::{Result, anyhow};
use nu_protocol::{Record, engine::Closure};

use crate::commands::{Perms, run_command, run_command_for_stdout};
use crate::parser::Engine;

use super::Backend;

const PACKAGE_LIST_KEY: &str = "packages";
const PACKAGE_KEY: &str = "package";
const ALL_FEATURES_KEY: &str = "all_features";
const NO_DEFAULT_FEATURES_KEY: &str = "no_default_features";
const FEATURES_KEY: &str = "features";
const GIT_REMOTE_KEY: &str = "git_remote";
const HOOK_KEY: &str = "post_hook";
const CRATE_INSTALLS_KEY: &str = "installs";

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
            .get(PACKAGE_LIST_KEY)
            .ok_or(anyhow!("Failed to get packages for Cargo"))?
            .as_list()
            .map_err(|_| anyhow!("Packages not a list for Cargo"))?
            .iter()
            .map(value_to_pkgspec)
            .collect::<Result<_>>()?;

        log::info!("Parsed cargo packages from spec");

        Ok(Cargo { packages })
    }

    fn install(&self, engine: &mut Engine, _config: &mut Record) -> Result<()> {
        let packages = get_installed_packages();
        log::info!("Successfully parsed installed packages");

        let configured_packages = &self.packages;
        let missing_packages = configured_packages
            .iter()
            .filter(|(name, _)| !packages.contains(*name));

        log::info!("Successfully found missing packages");

        let mut post_hooks = Vec::new();

        missing_packages
            .map(|(name, spec)| {
                spec.post_hook.as_ref().map(|hook| post_hooks.push(hook));
                install_package(name, spec)
            })
            .collect::<Result<()>>()?;

        log::info!("Successfully installed missing packages");

        let result = post_hooks
            .into_iter()
            .map(|hook| engine.execute_closure(hook))
            .collect();

        log::info!("Successfully executed all the post hooks");

        result
    }

    fn remove(&self, _config: &mut Record) -> Result<()> {
        let packages = get_installed_packages();
        log::info!("Successfully parsed installed packages");

        let configured_packages = &self.packages;
        packages
            .into_iter()
            .filter(|package| !configured_packages.contains_key(package))
            .map(|package| run_command(["cargo", "uninstall", package.as_str()], Perms::User))
            .collect::<Result<()>>()?;

        log::info!("Successfully removed extraneous packages");

        Ok(())
    }

    fn clean_cache(&self, _config: &Record) -> Result<()> {
        let stdout = run_command_for_stdout(["cargo", "cache", "--help"], Perms::User, false);

        match stdout {
            Ok(_) => {
                run_command(["cargo", "cache", "--autoclean"], Perms::User)?;
                log::info!("Removed cargo's cache");
            }
            Err(_) => {
                log::warn!("cargo-cache not found");
            }
        }

        Ok(())
    }
}

fn value_to_pkgspec(value: &nu_protocol::Value) -> Result<(String, CargoOpts)> {
    let record = value.as_record()?;

    let package = record
        .get(PACKAGE_KEY)
        .ok_or(anyhow!("No package mentioned"))?
        .as_str()
        .map_err(|_| anyhow!("Package name in record is not a string"))?
        .to_owned();

    let all_features = record
        .get(ALL_FEATURES_KEY)
        .and_then(|value| {
            value.as_bool().ok().or_else(|| {
                log::warn!("all_features in record is not a boolean, ignoring");
                None
            })
        })
        .unwrap_or_else(|| {
            log::info!("all_features not specified in record, defaulting to false");
            false
        });

    let no_default_features = if all_features {
        log::info!("all_features specified; ignoring no_default_features if present");
        false
    } else {
        record
            .get(NO_DEFAULT_FEATURES_KEY)
            .and_then(|value| {
                value.as_bool().ok().or_else(|| {
                    log::warn!("no_default_features in record is not a boolean, ignoring");
                    None
                })
            })
            .unwrap_or_else(|| {
                log::info!("no_default_features not specified in record, defaulting to false");
                false
            })
    };

    let features = if all_features || no_default_features {
        log::info!(
            "Either all_features or no_default_features is specified, ignoring features if any"
        );
        Box::new([])
    } else {
        record
            .get(FEATURES_KEY)
            .and_then(|value| {
                value.as_list().ok().or_else(|| {
                    log::warn!("features in record is not a list, ignoring");
                    None
                })
            })
            .map(|list| {
                list.iter()
                    .filter_map(|elem| {
                        elem.as_str().ok().or_else(|| {
                            log::warn!("feature in record is not a string, ignoring");
                            None
                        })
                    })
                    .map(ToOwned::to_owned)
                    .collect::<Box<[_]>>()
            })
            .unwrap_or_else(|| Box::new([]))
    };

    let git_remote = record
        .get(GIT_REMOTE_KEY)
        .and_then(|value| {
            value.as_str().ok().or_else(|| {
                log::warn!("git_remote in record is not a string, ignoring");
                None
            })
        })
        .map(ToOwned::to_owned);

    let post_hook = record
        .get(HOOK_KEY)
        .and_then(|closure| {
            closure.as_closure().ok().or_else(|| {
                log::warn!("post_hook in record is not a closure, ignoring");
                None
            })
        })
        .and_then(|post_hook| {
            if !post_hook.captures.is_empty() {
                log::warn!("closure captures variables, ignoring");
                None
            } else {
                Some(post_hook.to_owned())
            }
        });

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
        Err(_) => {
            log::warn!("Error occured in reading crate file. Assuming crates are not installed.");
            return HashSet::new();
        }
    };

    let cratespec: serde_json::Value = match serde_json::from_str(&cratespec) {
        Ok(cratespec) => cratespec,
        Err(_) => {
            log::warn!("Error occured in parsing json data. Ignoring the contents");
            return HashSet::new();
        }
    };

    log::info!("Found installed packages from crates2.json");

    let packages: HashSet<_> = cratespec
        .get(CRATE_INSTALLS_KEY)
        .or_else(|| {
            log::warn!("Malformed cratespec contents! Ignoring");
            None
        })
        .and_then(|value| value.as_object())
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

// TODO: Hopefully we'll eventually be able to use the spec to determine if there are any differences
// rather than just check for the existence of the package and leave it at that
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

#[cfg(test)]
mod test {
    use nu_protocol::{Id, Span, Value};

    use super::*;

    #[test]
    fn cargo_backend_ok() {
        let pkg_record = Record::from_raw_cols_vals(
            vec!["package".to_owned()],
            vec![Value::string("foo", Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();
        let pkglist = Value::list(
            vec![Value::record(pkg_record, Span::test_data())],
            Span::test_data(),
        );
        let record = Record::from_raw_cols_vals(
            vec!["packages".to_owned()],
            vec![pkglist],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let cargo = Cargo::new(&record);
        assert!(cargo.is_ok());
        let cargo = cargo.unwrap();
        assert_eq!(cargo.packages.len(), 1);
        assert!(cargo.packages.contains_key("foo"));
    }

    #[test]
    fn cargo_backend_not_list() {
        let pkg_record = Record::from_raw_cols_vals(
            vec!["package".to_owned()],
            vec![Value::string("foo", Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();
        let record = Record::from_raw_cols_vals(
            vec!["packages".to_owned()],
            vec![Value::record(pkg_record, Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let cargo = Cargo::new(&record);
        assert!(cargo.is_err());
    }

    #[test]
    fn cargo_backend_entry_missing() {
        let pkg_record = Record::from_raw_cols_vals(
            vec!["package".to_owned()],
            vec![Value::string("foo", Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();
        let record = Record::from_raw_cols_vals(
            vec!["packages".to_owned()],
            vec![Value::record(pkg_record, Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let cargo = Cargo::new(&record);
        assert!(cargo.is_err());
    }

    #[test]
    fn value_to_pkgspec_no_opts() {
        let record = Record::from_raw_cols_vals(
            vec!["package".to_owned()],
            vec![Value::string("foo", Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let res = value_to_pkgspec(&Value::record(record, Span::test_data()));
        assert!(res.is_ok());

        let res = res.unwrap();
        assert_eq!(res.0, "foo".to_string());
        let feats: [String; 0] = [];
        assert_eq!(*res.1.features, feats);
        assert_eq!(res.1.all_features, false);
        assert_eq!(res.1.no_default_features, false);
        assert_eq!(res.1.git_remote, None);
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn value_to_pkgspec_git() {
        let record = Record::from_raw_cols_vals(
            ["package", "git_remote"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::string("foo", Span::test_data()),
                Value::string("git_remote_example", Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let res = value_to_pkgspec(&Value::record(record, Span::test_data()));
        assert!(res.is_ok());

        let res = res.unwrap();
        assert_eq!(res.0, "foo".to_string());
        let feats: [String; 0] = [];
        assert_eq!(*res.1.features, feats);
        assert_eq!(res.1.all_features, false);
        assert_eq!(res.1.no_default_features, false);
        assert_eq!(res.1.git_remote, Some("git_remote_example".to_owned()));
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn value_to_pkgspec_all_feats() {
        let record = Record::from_raw_cols_vals(
            ["package", "all_features"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::string("foo", Span::test_data()),
                Value::bool(true, Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let res = value_to_pkgspec(&Value::record(record, Span::test_data()));
        assert!(res.is_ok());

        let res = res.unwrap();
        assert_eq!(res.0, "foo".to_string());
        let feats: [String; 0] = [];
        assert_eq!(*res.1.features, feats);
        assert_eq!(res.1.all_features, true);
        assert_eq!(res.1.no_default_features, false);
        assert_eq!(res.1.git_remote, None);
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn value_to_pkgspec_no_feats() {
        let record = Record::from_raw_cols_vals(
            ["package", "no_default_features"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::string("foo", Span::test_data()),
                Value::bool(true, Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let res = value_to_pkgspec(&Value::record(record, Span::test_data()));
        assert!(res.is_ok());

        let res = res.unwrap();
        assert_eq!(res.0, "foo".to_string());
        let feats: [String; 0] = [];
        assert_eq!(*res.1.features, feats);
        assert_eq!(res.1.all_features, false);
        assert_eq!(res.1.no_default_features, true);
        assert_eq!(res.1.git_remote, None);
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn value_to_pkgspec_all_feats_overrides_no_feats() {
        let record = Record::from_raw_cols_vals(
            ["package", "no_default_features", "all_features"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::string("foo", Span::test_data()),
                Value::bool(true, Span::test_data()),
                Value::bool(true, Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let res = value_to_pkgspec(&Value::record(record, Span::test_data()));
        assert!(res.is_ok());

        let res = res.unwrap();
        assert_eq!(res.0, "foo".to_string());
        let feats: [String; 0] = [];
        assert_eq!(*res.1.features, feats);
        assert_eq!(res.1.all_features, true);
        assert_eq!(res.1.no_default_features, false);
        assert_eq!(res.1.git_remote, None);
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn value_to_pkgspec_feats() {
        let record = Record::from_raw_cols_vals(
            ["package", "features"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::string("foo", Span::test_data()),
                Value::list(
                    vec![Value::string("bar", Span::test_data())],
                    Span::test_data(),
                ),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let res = value_to_pkgspec(&Value::record(record, Span::test_data()));
        assert!(res.is_ok());

        let res = res.unwrap();
        assert_eq!(res.0, "foo".to_string());
        let feats: [String; 1] = ["bar".to_owned()];
        assert_eq!(*res.1.features, feats);
        assert_eq!(res.1.all_features, false);
        assert_eq!(res.1.no_default_features, false);
        assert_eq!(res.1.git_remote, None);
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn value_to_pkgspec_no_feats_overrides_feats() {
        let record = Record::from_raw_cols_vals(
            ["package", "features", "no_default_features"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::string("foo", Span::test_data()),
                Value::list(
                    vec![Value::string("bar", Span::test_data())],
                    Span::test_data(),
                ),
                Value::bool(true, Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let res = value_to_pkgspec(&Value::record(record, Span::test_data()));
        assert!(res.is_ok());

        let res = res.unwrap();
        assert_eq!(res.0, "foo".to_string());
        let feats: [String; 0] = [];
        assert_eq!(*res.1.features, feats);
        assert_eq!(res.1.all_features, false);
        assert_eq!(res.1.no_default_features, true);
        assert_eq!(res.1.git_remote, None);
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn value_to_pkgspec_all_feats_overrides_feats() {
        let record = Record::from_raw_cols_vals(
            ["package", "features", "all_features"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::string("foo", Span::test_data()),
                Value::list(
                    vec![Value::string("bar", Span::test_data())],
                    Span::test_data(),
                ),
                Value::bool(true, Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let res = value_to_pkgspec(&Value::record(record, Span::test_data()));
        assert!(res.is_ok());

        let res = res.unwrap();
        assert_eq!(res.0, "foo".to_string());
        let feats: [String; 0] = [];
        assert_eq!(*res.1.features, feats);
        assert_eq!(res.1.all_features, true);
        assert_eq!(res.1.no_default_features, false);
        assert_eq!(res.1.git_remote, None);
        assert!(res.1.post_hook.is_none());
    }

    #[test]
    fn value_to_pkgspec_closure() {
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
                Value::closure(closure, Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let res = value_to_pkgspec(&Value::record(record, Span::test_data()));
        assert!(res.is_ok());

        let res = res.unwrap();
        assert_eq!(res.0, "foo".to_string());
        let feats: [String; 0] = [];
        assert_eq!(*res.1.features, feats);
        assert_eq!(res.1.all_features, false);
        assert_eq!(res.1.no_default_features, false);
        assert_eq!(res.1.git_remote, None);
        assert!(res.1.post_hook.is_some());
    }

    #[test]
    fn value_to_pkgspec_bound_closure() {
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
                Value::closure(closure, Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let res = value_to_pkgspec(&Value::record(record, Span::test_data()));
        assert!(res.is_ok());

        let res = res.unwrap();
        assert_eq!(res.0, "foo".to_string());
        let feats: [String; 0] = [];
        assert_eq!(*res.1.features, feats);
        assert_eq!(res.1.all_features, false);
        assert_eq!(res.1.no_default_features, false);
        assert_eq!(res.1.git_remote, None);
        assert!(res.1.post_hook.is_none());
    }
}
