use std::collections::HashMap;

use anyhow::{Result, anyhow};
use nu_protocol::{Record, Value};

use crate::{
    CleanCommand,
    commands::{Perms, dry_run_command, run_command, run_command_for_stdout},
    function, mod_err, nest_errors,
    parser::Engine,
};

use super::Backend;

const TOOLCHAIN_LIST_KEY: &str = "toolchains";
const COMPONENT_LIST_KEY: &str = "components";
const TARGET_LIST_KEY: &str = "targets";
const ARCH_KEY: &str = "arch";
const VENDOR_KEY: &str = "vendor";
const OS_KEY: &str = "os";

const DEFAULT_COMPONENTS: [&str; 7] = [
    "cargo",
    "clippy",
    "rust-docs",
    "rust-std",
    "rust-src",
    "rustc",
    "rustfmt",
];

#[derive(Debug, Clone)]
pub struct Rustup {
    toolchains: HashMap<String, ToolchainSpec>,
}

#[derive(Debug, Clone)]
struct ToolchainSpec {
    targets: Box<[String]>,
    components: Box<[String]>,
}

impl Backend for Rustup {
    fn new(value: &Record, _config: &Record) -> Result<Self> {
        let toolchains = value
            .get(TOOLCHAIN_LIST_KEY)
            .ok_or(mod_err!("Failed to get toolchains for Rustup"))?
            .as_record()
            .map_err(|e| nest_errors!("The toolchain spec in Rustup is not a record", e))?;

        let toolchains = values_to_pkgspec(toolchains)?;

        log::info!("Successfully parsed rustup packages");
        Ok(Rustup { toolchains })
    }

    fn install(&self, _engine: &mut Engine) -> Result<()> {
        let installed_toolchains = get_installed_toolchains()?;

        self.install_toolchains(installed_toolchains.as_ref())?;
        log::info!("Installed missing toolchains");

        self.install_missing(installed_toolchains.as_ref())?;
        log::info!("Installed missing components and targets");

        Ok(())
    }

    fn remove(&self, opts: &CleanCommand) -> Result<()> {
        let installed_toolchains = get_installed_toolchains()?;

        self.remove_toolchains(installed_toolchains.as_ref(), opts)?;
        log::info!("Removed extra toolchains");

        self.remove_extra(installed_toolchains.as_ref(), opts)?;
        log::info!("Removed extra components and targets");

        Ok(())
    }

    fn clean_cache(&self, _config: &Record) -> Result<()> {
        // Nothing to do here
        Ok(())
    }
}

impl Rustup {
    fn install_toolchains(&self, installed_toolchains: &[String]) -> Result<()> {
        let configured_toolchains = self.toolchains.keys();

        configured_toolchains
            .filter(|toolchain| {
                !installed_toolchains
                    .iter()
                    .any(|installed| installed.starts_with(*toolchain))
            })
            .map(|toolchain| (toolchain, self.toolchains.get(toolchain).unwrap()))
            .try_for_each(|(toolchain, spec)| install_missing_toolchain(toolchain, spec))
            .inspect(|_| log::debug!("Successfully installed all the missing toolchains"))
    }

    fn install_missing(&self, installed_toolchains: &[String]) -> Result<()> {
        let configured_toolchains = installed_toolchains.iter().filter_map(|toolchain| {
            self.toolchains
                .keys()
                .find(|configured| toolchain.starts_with(*configured))
        });

        for toolchain in configured_toolchains {
            let toolchain_spec = self.toolchains.get(toolchain).unwrap();

            install_missing_targets(toolchain, toolchain_spec.targets.as_ref())?;
            install_missing_components(toolchain, toolchain_spec.components.as_ref())?;
        }

        Ok(())
    }

    fn remove_toolchains(
        &self,
        installed_toolchains: &[String],
        opts: &CleanCommand,
    ) -> Result<()> {
        let configured_toolchains = &self.toolchains;

        let mut extra_toolchains = installed_toolchains
            .iter()
            .filter(|toolchain| {
                !configured_toolchains
                    .keys()
                    .into_iter()
                    .any(|configured| toolchain.starts_with(configured))
            })
            .map(String::as_str)
            .peekable();

        let command_action = if opts.dry_run {
            dry_run_command
        } else {
            run_command
        };

        if extra_toolchains.peek().is_none() {
            log::debug!("No extra toolchains to remove!");
            Ok(())
        } else {
            command_action(
                ["rustup", "toolchain", "remove"]
                    .into_iter()
                    .chain(extra_toolchains),
                Perms::User,
            )
            .map(|_| log::debug!("Successfully removed unused toolchains"))
            .map_err(|e| nest_errors!("Failed to remove toolchains", e))
        }
    }

    fn remove_extra(&self, installed_toolchains: &[String], opts: &CleanCommand) -> Result<()> {
        let configured_toolchains = &self.toolchains;

        let present_toolchains = installed_toolchains.iter().flat_map(|toolchain| {
            configured_toolchains
                .keys()
                .find(|configured| toolchain.starts_with(*configured))
        });

        for toolchain in present_toolchains {
            let toolchain_spec = self.toolchains.get(toolchain).unwrap();

            remove_extra_targets(toolchain, &toolchain_spec.targets, opts)?;
            remove_extra_components(toolchain, &toolchain_spec.components, opts)?;
        }

        Ok(())
    }
}

fn values_to_pkgspec(record: &Record) -> Result<HashMap<String, ToolchainSpec>> {
    record
        .iter()
        .map(|(toolchain, value)| -> Result<_> {
            Ok((
                toolchain.to_owned(),
                value_to_toolchainspec(toolchain, value)?,
            ))
        })
        .collect()
}

fn get_installed_toolchains() -> Result<Box<[String]>> {
    let toolchains = run_command_for_stdout(["rustup", "toolchain", "list"], Perms::User, true)
        .map_err(|e| nest_errors!("Failed to get toolchains", e))?;

    let toolchains = toolchains
        .lines()
        .map(|toolchain| toolchain.split_once(' ').map_or(toolchain, |split| split.0))
        .map(ToOwned::to_owned)
        .collect();

    Ok(toolchains)
}

fn install_missing_toolchain(toolchain: &str, toolchain_spec: &ToolchainSpec) -> Result<()> {
    let components = Some(
        ["--component"]
            .into_iter()
            .chain(toolchain_spec.components.iter().map(String::as_str)),
    )
    .into_iter()
    .filter(|_| !toolchain_spec.components.is_empty())
    .flatten();

    let targets = Some(
        ["--target"]
            .into_iter()
            .chain(toolchain_spec.targets.iter().map(String::as_str)),
    )
    .into_iter()
    .filter(|_| !toolchain_spec.targets.is_empty())
    .flatten();

    run_command(
        ["rustup", "toolchain", "install"]
            .into_iter()
            .chain(components)
            .chain(targets),
        Perms::User,
    )
    .inspect(|_| log::debug!("Successfully installed missing toolchain {toolchain}"))
    .map_err(|e| nest_errors!("Failed to install toolchain {toolchain}", e))
}

fn install_missing_targets(toolchain: &String, configured_targets: &[String]) -> Result<()> {
    let installed_targets = get_installed_targets(toolchain)?;

    let mut missing_targets = configured_targets
        .iter()
        .filter(|target| !installed_targets.contains(target))
        .map(String::as_str)
        .peekable();

    if missing_targets.peek().is_none() {
        log::debug!("No targets left to install for {toolchain}!");
        Ok(())
    } else {
        run_command(
            ["rustup", "target", "add", "--toolchain", toolchain]
                .into_iter()
                .chain(missing_targets),
            Perms::User,
        )
        .map_err(|e| nest_errors!("Failed to add targets for {toolchain}", e))
    }
}

fn install_missing_components(toolchain: &String, configured_components: &[String]) -> Result<()> {
    let installed_components = get_installed_components(toolchain)?;

    let mut missing_components = configured_components
        .iter()
        .map(String::as_str)
        .chain(DEFAULT_COMPONENTS)
        .filter(|component| {
            !installed_components
                .iter()
                .any(|comp| comp.starts_with(*component))
        })
        .peekable();

    if missing_components.peek().is_none() {
        log::debug!("No components left to install for {toolchain}");
        Ok(())
    } else {
        run_command(
            ["rustup", "component", "add", "--toolchain", toolchain]
                .into_iter()
                .chain(missing_components),
            Perms::User,
        )
        .map_err(|e| nest_errors!("Failed to add components for {toolchain}", e))
    }
}

fn remove_extra_targets(
    toolchain: &str,
    configured_targets: &[String],
    opts: &CleanCommand,
) -> Result<()> {
    let installed_targets = get_installed_targets(toolchain)?;

    let mut extra_targets = installed_targets
        .iter()
        .filter(|target| !configured_targets.contains(target))
        .map(String::as_str)
        .peekable();

    let command_action = if opts.dry_run {
        dry_run_command
    } else {
        run_command
    };

    if extra_targets.peek().is_none() {
        log::debug!("No extra targets to remove for {toolchain}!");
        Ok(())
    } else {
        command_action(
            ["rustup", "target", "remove", "--toolchain", toolchain]
                .into_iter()
                .chain(extra_targets),
            Perms::User,
        )
        .inspect(|_| log::debug!("Remove extra targets for {toolchain}"))
        .map_err(|e| nest_errors!("Failed to remove unused targets for {toolchain}", e))
    }
}

fn remove_extra_components(
    toolchain: &str,
    configured_components: &[String],
    opts: &CleanCommand,
) -> Result<()> {
    let installed_components = get_installed_components(toolchain)?;

    let mut extra_components = installed_components
        .iter()
        .filter(|component| {
            !configured_components
                .iter()
                .map(String::as_str)
                .chain(DEFAULT_COMPONENTS)
                .any(|comp| component.starts_with(comp))
        })
        .map(String::as_str)
        .peekable();

    let command_action = if opts.dry_run {
        dry_run_command
    } else {
        run_command
    };

    if extra_components.peek().is_none() {
        log::debug!("No extra components to remove for {toolchain}!");
        Ok(())
    } else {
        command_action(
            ["rustup", "component", "remove", "--toolchain", toolchain]
                .into_iter()
                .chain(extra_components),
            Perms::User,
        )
        .inspect(|_| log::debug!("Removed extra components for {toolchain}"))
        .map_err(|e| {
            nest_errors!(
                "rustup command to remove components for {toolchain} failed!",
                e
            )
        })
    }
}

fn value_to_toolchainspec(toolchain: &str, value: &Value) -> Result<ToolchainSpec> {
    let record = value
        .as_record()
        .map_err(|e| nest_errors!("Parse error for value in {toolchain}, not a record", e))?;

    let components = match record.get(COMPONENT_LIST_KEY) {
        Some(components) => {
            let components = components
                .as_list()
                .map_err(|e| nest_errors!("Failed to convert components to a list", e))?;

            parse_components(components)?
        }
        None => {
            log::debug!("No components specified in {toolchain}, using defaults");
            Box::new([])
        }
    };

    let targets: Box<[_]> = match record.get(TARGET_LIST_KEY) {
        Some(targets) => {
            let targets = targets.as_list().map_err(|e| {
                nest_errors!("Parse error for targets in {toolchain}, not a list", e)
            })?;
            targets
                .iter()
                .map(|target| parse_target(target, toolchain))
                .collect::<Result<_>>()?
        }
        None => {
            log::debug!("No targets specified in {toolchain}, using defaults");
            Box::new([])
        }
    };

    Ok(ToolchainSpec {
        targets,
        components,
    })
}

fn get_installed_targets(toolchain: &str) -> Result<Box<[String]>> {
    let targets = run_command_for_stdout(
        [
            "rustup",
            "target",
            "list",
            "--toolchain",
            toolchain,
            "--installed",
            "--quiet",
        ],
        Perms::User,
        false,
    )
    .map_err(|e| nest_errors!("rustup command to find targets for {toolchain} failed", e))?;

    let targets = targets
        .lines()
        .map(str::trim)
        .map(ToOwned::to_owned)
        .collect();

    Ok(targets)
}

fn get_installed_components(toolchain: &str) -> Result<Box<[String]>> {
    let components = run_command_for_stdout(
        [
            "rustup",
            "component",
            "list",
            "--toolchain",
            toolchain,
            "--installed",
            "--quiet",
        ],
        Perms::User,
        false,
    )
    .map_err(|e| {
        nest_errors!(
            "rustup command to find components for {toolchain} failed",
            e
        )
    })?;

    let components = components
        .lines()
        .map(str::trim)
        .map(ToOwned::to_owned)
        .collect();

    Ok(components)
}

fn parse_components(components: &[Value]) -> Result<Box<[String]>> {
    components
        .iter()
        .map(|comp| {
            comp.as_str()
                .map(ToOwned::to_owned)
                .map_err(|e| nest_errors!("Expected a string for component\n", e))
        })
        .collect()
}

fn parse_target(target: &Value, toolchain: &str) -> Result<String> {
    let target = target
        .as_record()
        .map_err(|e| nest_errors!("Specified target for {toolchain} not a record", e))?;

    let arch = target
        .get(ARCH_KEY)
        .ok_or(mod_err!(
            "Failed to get architecture from target for {toolchain}"
        ))?
        .as_str()
        .map_err(|e| nest_errors!("Architecture specified is not a string", e))?;

    let vendor = target
        .get(VENDOR_KEY)
        .map(|vendor| {
            vendor.as_str().map_err(|e| {
                nest_errors!(
                    "Vendor specified is not a string for {arch} in {toolchain}",
                    e
                )
            })
        })
        .unwrap_or_else(|| {
            log::debug!("Using default value for vendor in {toolchain}'s {arch}");
            Ok("unknown")
        })?;

    let os = target
        .get(OS_KEY)
        .map(|os| {
            os.as_str().map_err(|e| {
                nest_errors!("OS specified is not a string for {arch} in {toolchain}", e)
            })
        })
        .unwrap_or_else(|| {
            log::debug!("Using default value for vendor in {toolchain}'s {arch}");
            Ok("none")
        })?;

    Ok([arch, vendor, os].join("-"))
}

#[cfg(test)]
mod test {
    use nu_protocol::Span;

    use super::*;

    #[test]
    fn rustup_backend_ok() {
        let component_list = vec![Value::string("foo", Span::test_data())];

        let target = Record::from_raw_cols_vals(
            ["arch", "vendor", "os"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            ["AMD64", "unknown-elf", "linux"]
                .into_iter()
                .map(|string| Value::string(string, Span::test_data()))
                .collect(),
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();
        let target_list = vec![Value::record(target, Span::test_data())];

        let inner_record = Record::from_raw_cols_vals(
            ["components", "targets"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            vec![
                Value::list(component_list, Span::test_data()),
                Value::list(target_list, Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let toolchain_record = Record::from_raw_cols_vals(
            vec!["toolchain1".to_owned()],
            vec![Value::record(inner_record, Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let outer_record = Record::from_raw_cols_vals(
            vec!["toolchains".to_owned()],
            vec![Value::record(toolchain_record, Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let result = Rustup::new(&outer_record, &Record::new());
        assert!(result.is_ok());
    }

    #[test]
    fn rustup_backend_not_record() {
        let outer_record = Record::from_raw_cols_vals(
            vec!["toolchains".to_owned()],
            vec![Value::string("foo", Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let result = Rustup::new(&outer_record, &Record::new());
        assert!(result.is_err());
    }

    #[test]
    fn values_to_fields_ok() {
        let target = Record::from_raw_cols_vals(
            ["arch", "vendor", "os"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            ["AMD64", "unknown-elf", "linux"]
                .into_iter()
                .map(|string| Value::string(string, Span::test_data()))
                .collect(),
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();
        let target_list = vec![Value::record(target, Span::test_data())];
        let component_list = vec![Value::string("foo", Span::test_data())];

        let inner_record = Record::from_raw_cols_vals(
            vec!["components".to_owned(), "targets".to_owned()],
            vec![
                Value::list(component_list, Span::test_data()),
                Value::list(target_list, Span::test_data()),
            ],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let result = value_to_toolchainspec("_", &Value::record(inner_record, Span::test_data()));
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(*result.targets, ["AMD64-unknown-elf-linux"]);
        assert_eq!(*result.components, ["foo"]);
    }

    #[test]
    fn values_to_fields_components_missing() {
        let target = Record::from_raw_cols_vals(
            ["arch", "vendor", "os"]
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            ["AMD64", "unknown-elf", "linux"]
                .into_iter()
                .map(|string| Value::string(string, Span::test_data()))
                .collect(),
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();
        let target_list = vec![Value::record(target, Span::test_data())];

        let inner_record = Record::from_raw_cols_vals(
            vec!["targets".to_owned()],
            vec![Value::list(target_list, Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let result = value_to_toolchainspec("_", &Value::record(inner_record, Span::test_data()));
        assert!(result.is_ok());
        let result = result.unwrap();
        let res: [String; 0] = [];
        assert_eq!(*result.components, res);
    }

    #[test]
    fn values_to_fields_targets_missing() {
        let component_list = vec![Value::string("foo", Span::test_data())];

        let inner_record = Record::from_raw_cols_vals(
            vec!["components".to_owned()],
            vec![Value::list(component_list, Span::test_data())],
            Span::test_data(),
            Span::test_data(),
        )
        .unwrap();

        let result = value_to_toolchainspec("_", &Value::record(inner_record, Span::test_data()));

        assert!(result.is_ok());
        let res: [String; 0] = [];
        let result = result.unwrap();
        assert_eq!(*result.targets, res);
    }

    #[test]
    fn values_to_fields_not_record() {
        let result = value_to_toolchainspec("_", &Value::string("foo", Span::test_data()));
        assert!(result.is_err());
        // let res: [String; 0] = [];
        // assert_eq!(*result.0.0, res);
        // assert_eq!(*result.1.0, res);
    }

    #[test]
    fn parse_components_ok() {
        let components: Vec<_> = ["foo", "bar", "aaaa"]
            .into_iter()
            .map(|comp| Value::string(comp, Span::test_data()))
            .collect();

        let result = parse_components(&components);
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], "foo");
        assert_eq!(result[1], "bar");
        assert_eq!(result[2], "aaaa");
    }

    #[test]
    fn parse_components_bad() {
        let mut components: Vec<_> = ["foo", "bar", "aaaa"]
            .into_iter()
            .map(|comp| Value::string(comp, Span::test_data()))
            .collect();

        components.push(Value::bool(true, Span::test_data()));

        let result = parse_components(&components);
        assert!(result.is_err());
    }
}
