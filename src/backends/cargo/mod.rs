use std::collections::{HashMap, HashSet};
use std::fs;

use anyhow::{Context, Result, anyhow};
use nu_protocol::{Record, engine::Closure};

use crate::commands::{
    Perms, confirmation_prompt, dry_run_command, run_command, run_command_for_stdout,
};
use crate::config::{CARGO_USE_BINSTALL_KEY, DEFAULT_CARGO_USE_BINSTALL};
use crate::parser::Engine;
use crate::{CleanCacheCommand, CleanCommand, SyncCommand, function, mod_err, nest_errors};

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
    installopt: &'static str,
}

impl Backend for Cargo {
    fn new(value: &Record, config: &Record) -> Result<Self> {
        let packages = value
            .get(PACKAGE_LIST_KEY)
            .ok_or(mod_err!("Failed to get packages for Cargo"))?
            .as_list()
            .map_err(|e| nest_errors!("Packages not a list for Cargo", e))?
            .iter()
            .map(value_to_pkgspec)
            .collect::<Result<_>>()?;

        log::info!("Parsed cargo packages from spec");

        let installopt = if get_binstall_opt(config)? {
            "binstall"
        } else {
            "install"
        };

        Ok(Cargo {
            packages,
            installopt,
        })
    }

    fn install(&self, engine: &mut Engine, opts: &SyncCommand) -> Result<()> {
        let packages = get_installed_packages()?;

        let configured_packages = &self.packages;
        let missing_packages: HashMap<_, _> = configured_packages
            .iter()
            .filter(|(name, _)| !packages.contains(*name))
            .collect();

        let mut post_hooks = Vec::new();

        if !opts.no_confirm
            && !confirmation_prompt(
                "Do you want to install the following packages?: ",
                missing_packages.keys(),
            )?
        {
            return Ok(());
        }

        missing_packages.iter().try_for_each(|(name, spec)| {
            if let Some(hook) = spec.post_hook.as_ref() {
                post_hooks.push(hook);
            }
            install_package(name, spec, self.installopt, opts)
        })?;

        log::info!("Successfully installed missing packages");

        post_hooks
            .into_iter()
            .try_for_each(|hook| {
                if opts.dry_run {
                    engine.dry_run_closure(hook)
                } else {
                    engine.execute_closure(hook)
                }
            })
            .inspect(|_| log::info!("Successfully executed all the post hooks"))
    }

    fn remove(&self, opts: &CleanCommand) -> Result<()> {
        let packages = get_installed_packages()?;
        log::info!("Successfully parsed installed packages");

        let configured_packages = &self.packages;

        let command_action: fn([&str; 3], Perms) -> Result<()> = if opts.dry_run {
            |args, perms| dry_run_command(args, perms)
        } else {
            |args, perms| run_command(args, perms)
        };

        let extra_packages: HashSet<_> = packages
            .into_iter()
            .filter(|package| !configured_packages.contains_key(package))
            .collect();

        if !opts.no_confirm
            && !confirmation_prompt(
                "Do you want to remove the following packages?: ",
                &extra_packages,
            )?
        {
            return Ok(());
        }

        extra_packages
            .iter()
            .try_for_each(|package| {
                command_action(["cargo", "uninstall", package.as_str()], Perms::User)
                    .map_err(|e| nest_errors!("Failed to uninstall {package}", e))
            })
            .inspect(|_| log::info!("Successfully removed extraneous packages"))
    }

    fn clean_cache(&self, _config: &Record, opts: &CleanCacheCommand) -> Result<()> {
        let stdout = run_command_for_stdout(["cargo", "cache", "--help"], Perms::User, false);

        let command_action = if opts.dry_run {
            dry_run_command
        } else {
            run_command
        };

        if !opts.no_confirm
            && !confirmation_prompt(
                "Do you want to clean cargo cache?",
                ["Using cargo-cache subcommand"],
            )?
        {
            return Ok(());
        }

        match stdout {
            Ok(_) => {
                command_action(["cargo", "cache", "--autoclean"], Perms::User)
                    .map_err(|e| nest_errors!("Failed to remove cache", e))?;
                log::debug!("Removed cargo's cache");
            }
            Err(_) => {
                log::warn!("cargo-cache not found");
            }
        }

        Ok(())
    }
}

fn get_binstall_opt(config: &Record) -> Result<bool> {
    match config.get(CARGO_USE_BINSTALL_KEY) {
        Some(opt) => opt.as_bool().map_err(|e| {
            nest_errors!(
                "Failed to parse config, cargo binstall option not a bool",
                e
            )
        }),
        None => Ok(DEFAULT_CARGO_USE_BINSTALL),
    }
}

fn value_to_pkgspec(value: &nu_protocol::Value) -> Result<(String, CargoOpts)> {
    let record = value
        .as_record()
        .map_err(|e| nest_errors!("Failed to parse value", e))?;

    let package = record
        .get(PACKAGE_KEY)
        .ok_or(mod_err!("No package mentioned"))?
        .as_str()
        .map_err(|e| nest_errors!("Package name in record is not a string", e))?
        .to_owned();

    let all_features = match record.get(ALL_FEATURES_KEY) {
        Some(all_features) => all_features
            .as_bool()
            .map_err(|e| nest_errors!("all_features in {package} is not a boolean", e))?,
        None => {
            log::debug!("all_features not specified in {package}, defaulting to false");
            false
        }
    };

    let no_default_features = record
        .get(NO_DEFAULT_FEATURES_KEY)
        .filter(|_| !all_features);
    let no_default_features = match no_default_features {
        Some(no_default_features) => no_default_features
            .as_bool()
            .map_err(|e| nest_errors!("no_default_features in {package} is not a boolean", e))?,
        None => {
            log::debug!("no_default_features not specified in {package}, defaulting to false");
            false
        }
    };

    let features = record
        .get(FEATURES_KEY)
        .filter(|_| !all_features && !no_default_features);
    let features = match features {
        Some(features) => features
            .as_list()
            .map_err(|e| nest_errors!("features in {package} is not a list", e))?
            .iter()
            .map(|elem| {
                elem.as_str()
                    .map(ToOwned::to_owned)
                    .map_err(|e| nest_errors!("Element in {package} features not a string", e))
            })
            .collect::<Result<Box<[_]>>>()?,
        None => Box::new([]),
    };

    let git_remote = match record.get(GIT_REMOTE_KEY) {
        Some(git_remote) => Some(
            git_remote
                .as_str()
                .map_err(|e| nest_errors!("Failed to parse git remote for {package}", e))?
                .to_owned(),
        ),
        None => None,
    };

    let post_hook = match record.get(HOOK_KEY) {
        Some(closure) => {
            let closure = closure
                .as_closure()
                .map_err(|e| nest_errors!("closure for {package} not a closure", e))?;
            if !closure.captures.is_empty() {
                log::warn!("closure for {package} captures variables");
                None
            } else {
                Some(closure.to_owned())
            }
        }
        None => None,
    };

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

fn get_installed_packages() -> Result<HashSet<String>> {
    let crate_file = std::env::var("CARGO_HOME").or_else(|e| -> Result<String> {
        log::debug!("Encountered error: {e}");
        log::debug!("Using the default: ~/.cargo");
        let home = std::env::var("HOME")?;
        Ok(home + "/.cargo")
    })? + "/.crates2.json";

    let cratespec = match fs::read_to_string(&crate_file) {
        Ok(cratespec) => cratespec,
        Err(e) => {
            log::warn!(
                "Error {e} occured in reading crate file. Assuming crates are not installed."
            );
            return Ok(HashSet::new());
        }
    };

    let cratespec: serde_json::Value =
        serde_json::from_str(&cratespec).with_context(|| "error occured in parsing json data")?;

    let packages: HashSet<_> = cratespec
        .get(CRATE_INSTALLS_KEY)
        .ok_or_else(|| mod_err!("Malformed cratespec contents! Can't find the required installs"))?
        .as_object()
        .ok_or_else(|| mod_err!("Malformed cratespec contents! Installs field not a JSON object"))?
        .keys()
        .filter_map(|package| package.split_once(' ').map(|package| package.0))
        .map(ToOwned::to_owned)
        .collect();

    Ok(packages)
}

fn install_package(
    name: &str,
    spec: &CargoOpts,
    installer: &str,
    opts: &SyncCommand,
) -> Result<()> {
    let git = Some("--git")
        .into_iter()
        .chain(spec.git_remote.as_deref())
        .filter(|_| spec.git_remote.is_some());

    let all_features = Some("--all-features")
        .into_iter()
        .filter(|_| spec.all_features);

    let no_default_features = Some("--no-default-features")
        .into_iter()
        .filter(|_| spec.no_default_features);

    let features = Some("--features")
        .into_iter()
        .chain(spec.features.iter().map(String::as_str))
        .filter(|_| !spec.features.is_empty());

    let command = ["cargo", installer]
        .into_iter()
        .chain(git)
        .chain(all_features)
        .chain(no_default_features)
        .chain(features)
        .chain([name]);

    let command_action = if opts.dry_run {
        dry_run_command
    } else {
        run_command
    };

    command_action(command, Perms::User).map_err(|e| nest_errors!("Failed to install {name}", e))
}

// TODO: Hopefully we'll eventually be able to use the spec to determine if there are any differences
// rather than just check for the existence of the package and leave it at that
fn _cargospec_to_pkgspec(name: &str, spec: &serde_json::Value) -> Result<(String, CargoOpts)> {
    let spec = spec.as_object().ok_or(mod_err!("Malformed spec: {name}"))?;

    let (name, version_source) = name
        .split_once(' ')
        .ok_or(mod_err!("Malformed name: {name}"))?;

    let (_version, source) = version_source
        .split_once(' ')
        .ok_or(mod_err!("Malformed version/source: {name}"))?;

    let git_remote = if source.starts_with("(git+") {
        let url = source
            .split("+")
            .nth(1)
            .ok_or(mod_err!("Malformed git source: {name}"))?
            .split("#")
            .next()
            .ok_or(mod_err!("Malformed git url: {name}"))?
            .to_owned();

        Some(url)
    } else {
        None
    };

    let all_features = spec
        .get("all_features")
        .ok_or(mod_err!("Missing field all_features: {name}"))?
        .as_bool()
        .ok_or(mod_err!("Malformed field all_features not a bool: {name}"))?;

    let no_default_features = spec
        .get("no_default_features")
        .ok_or(mod_err!("Missing field all_features: {name}"))?
        .as_bool()
        .ok_or(mod_err!("Malformed field all_features not a bool: {name}"))?;

    let features = spec
        .get("features")
        .ok_or(mod_err!("Missing field features: {name}"))?
        .as_array()
        .ok_or(mod_err!("Malformed field features: {name}"))?
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

        let cargo = Cargo::new(&record, &Record::new());
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

        let cargo = Cargo::new(&record, &Record::new());
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

        let cargo = Cargo::new(&record, &Record::new());
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
        assert!(!res.1.all_features);
        assert!(!res.1.no_default_features);
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
        assert!(!res.1.all_features);
        assert!(!res.1.no_default_features);
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
        assert!(res.1.all_features);
        assert!(!res.1.no_default_features);
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
        assert!(!res.1.all_features);
        assert!(res.1.no_default_features);
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
        assert!(res.1.all_features);
        assert!(!res.1.no_default_features);
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
        assert!(!res.1.all_features);
        assert!(!res.1.no_default_features);
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
        assert!(!res.1.all_features);
        assert!(res.1.no_default_features);
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
        assert!(res.1.all_features);
        assert!(!res.1.no_default_features);
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
        assert!(!res.1.all_features);
        assert!(!res.1.no_default_features);
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
        assert!(!res.1.all_features);
        assert!(!res.1.no_default_features);
        assert_eq!(res.1.git_remote, None);
        assert!(res.1.post_hook.is_none());
    }
}
