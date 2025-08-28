use std::collections::{HashMap, HashSet};

use anyhow::anyhow;
use nu_protocol::{Record, Value};

use crate::{
    commands::{Perms, run_command, run_command_for_stdout},
    parser::Engine,
};

use super::Backend;

const TOOLCHAIN_LIST_KEY: &str = "toolchains";
const COMPONENT_LIST_KEY: &str = "components";
const TARGET_LIST_KEY: &str = "targets";
const ARCH_KEY: &str = "arch";
const VENDOR_KEY: &str = "vendor";
const OS_KEY: &str = "os";

#[derive(Debug, Clone)]
pub struct Rustup {
    toolchains: HashMap<String, (Targets, Components)>,
}

#[derive(Debug, Clone)]
struct Components(Box<[String]>);
#[derive(Debug, Clone)]
struct Targets(Box<[String]>);

impl Backend for Rustup {
    fn new(value: &Record) -> anyhow::Result<Self> {
        let toolchains = value
            .get(TOOLCHAIN_LIST_KEY)
            .or_else(|| {
                log::warn!("toolchains not listed for rustup");
                None
            })
            .and_then(|target| target.as_record().ok())
            .or_else(|| {
                log::warn!("rustup toolchains value not a record");
                None
            })
            .map(values_to_pkgspec)
            .ok_or(anyhow!("Failed to parse rustup packages"))?;

        Ok(Rustup { toolchains })
    }

    fn install(&self, engine: &mut Engine, config: &mut Record) -> anyhow::Result<()> {
        let installed_toolchains = get_installed_toolchains();

        self.install_toolchains(installed_toolchains.as_ref());
        self.install_missing(installed_toolchains.as_ref());

        Ok(())
    }

    fn remove(&self, config: &mut Record) -> anyhow::Result<()> {
        todo!()
    }

    fn clean_cache(&self, config: &Record) -> anyhow::Result<()> {
        todo!()
    }
}

impl Rustup {
    fn install_toolchains(&self, installed_toolchains: &[String]) {
        let configured_toolchains = self.toolchains.keys();

        let missing_toolchains =
            configured_toolchains.filter(|toolchain| !installed_toolchains.contains(*toolchain));

        for toolchain in missing_toolchains {
            let (components, targets) = self.toolchains.get(toolchain).unwrap();

            let components = Some(
                ["--component"]
                    .into_iter()
                    .chain(components.0.iter().map(String::as_str)),
            )
            .into_iter()
            .filter(|_| components.0.len() > 0)
            .flatten();

            let targets = Some(
                ["--component"]
                    .into_iter()
                    .chain(targets.0.iter().map(String::as_str)),
            )
            .into_iter()
            .filter(|_| targets.0.len() > 0)
            .flatten();

            let result = run_command(
                ["rustup", "toolchain", "install"]
                    .into_iter()
                    .chain(components)
                    .chain(targets),
                Perms::User,
            );

            match result {
                Ok(_) => log::info!("Successfully installed missing toolchain {toolchain}"),
                Err(_) => log::warn!("Failed to install toolchain {toolchain}, proceeeding ahead"),
            }
        }
    }

    fn install_missing(&self, installed_toolchains: &[String]) {
        for toolchain in installed_toolchains {
            let (configured_targets, configured_components) =
                self.toolchains.get(toolchain).unwrap();

            install_missing_targets(toolchain, configured_targets.0.as_ref());

            install_missing_components(toolchain, configured_components.0.as_ref());
        }
    }
}

fn values_to_pkgspec(record: &Record) -> HashMap<String, (Targets, Components)> {
    record
        .iter()
        .map(|(toolchain, value)| (toolchain.to_owned(), value_to_fields(value)))
        .collect()
}

fn get_installed_toolchains() -> Box<[String]> {
    run_command_for_stdout(["rustup", "toolchain", "list"], Perms::User, true)
        .ok()
        .or_else(|| {
            log::warn!("rustup command to find toolchains failed!");
            None
        })
        .iter()
        .flat_map(|output| output.lines())
        .map(|toolchain| toolchain.split_once(' ').map_or(toolchain, |split| split.0))
        .map(ToOwned::to_owned)
        .collect()
}

fn install_missing_targets(toolchain: &String, configured_targets: &[String]) {
    let installed_targets = run_command_for_stdout(
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
    .ok()
    .or_else(|| {
        log::warn!("rustup command to find targets for {toolchain} failed!");
        None
    })
    .unwrap_or_default();
    let installed_targets: Box<[_]> = installed_targets
        .lines()
        .map(|target| target.trim())
        .collect();

    let missing_components = configured_targets
        .iter()
        .filter(|component| !installed_targets.contains(&component.as_str()))
        .map(String::as_str);

    run_command(
        ["rustup", "component", "add", "--toolchain", toolchain]
            .into_iter()
            .chain(missing_components),
        Perms::User,
    )
    .ok()
    .or_else(|| {
        log::warn!("rustup command to add components for {toolchain} failed!");
        None
    });
}

fn install_missing_components(toolchain: &String, configured_components: &[String]) {
    let installed_components = run_command_for_stdout(
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
    .ok()
    .or_else(|| {
        log::warn!("rustup command to find components for {toolchain} failed!");
        None
    });

    let missing_components = configured_components
        .iter()
        .filter(|component| {
            !installed_components
                .iter()
                .flat_map(|output| output.lines())
                .any(|comp| comp.starts_with(*component))
        })
        .map(String::as_str);

    run_command(
        ["rustup", "component", "add", "--toolchain", toolchain]
            .into_iter()
            .chain(missing_components),
        Perms::User,
    )
    .ok()
    .or_else(|| {
        log::warn!("rustup command to add components for {toolchain} failed!");
        None
    });
}

fn value_to_fields(value: &Value) -> (Targets, Components) {
    match value.as_record() {
        Err(_) => {
            log::warn!("parse error for {value:#?}; not a record");
            (Targets(Box::new([])), Components(Box::new([])))
        }
        Ok(record) => {
            let components = record
                .get(COMPONENT_LIST_KEY)
                .or_else(|| {
                    log::debug!("Components not present for {record:#?}");
                    None
                })
                .and_then(|components| components.as_list().ok())
                .or_else(|| {
                    log::warn!("Component not a list for {record:#?}");
                    None
                })
                .map(parse_components)
                .unwrap_or(Box::new([]));

            let targets: Box<[_]> = record
                .get(TARGET_LIST_KEY)
                .or_else(|| {
                    log::debug!("Targets not present for {record:#?}");
                    None
                })
                .and_then(|targets| targets.as_list().ok())
                .or_else(|| {
                    log::warn!("Target not a list for {record:#?}");
                    None
                })
                .map(|targets| targets.iter().flat_map(parse_target).collect())
                .unwrap_or(Box::new([]));

            (Targets(targets), Components(components))
        }
    }
}

fn parse_components(components: &[Value]) -> Box<[String]> {
    components
        .iter()
        .flat_map(|comp| {
            comp.as_str().ok().or_else(|| {
                log::debug!("Expected a string for {comp:#?}");
                None
            })
        })
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_target(target: &Value) -> Option<String> {
    let target = target.as_record().ok()?;

    let arch = target
        .get(ARCH_KEY)
        .or_else(|| {
            log::warn!("Arch key missing for target {target:#?}");
            None
        })?
        .as_str()
        .ok()
        .or_else(|| {
            log::warn!("Arch not a string for target {target:#?}");
            None
        })?;
    let vendor = target
        .get(VENDOR_KEY)
        .or_else(|| {
            log::debug!("Vendor key is not present for target {target:#?}");
            None
        })
        .and_then(|vendor| vendor.as_str().ok())
        .or_else(|| {
            log::warn!("Vendor is not a string for target {target:#?}");
            None
        })
        .unwrap_or("unknown");
    let os = target
        .get(OS_KEY)
        .or_else(|| {
            log::debug!("Os key is not present for target {target:#?}");
            None
        })
        .and_then(|os| os.as_str().ok())
        .or_else(|| {
            log::warn!("Os is not a string for target {target:#?}");
            None
        })
        .unwrap_or("none");

    Some([arch, vendor, os].join("-"))
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

        let result = Rustup::new(&outer_record);
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

        let result = Rustup::new(&outer_record);
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

        let result = value_to_fields(&Value::record(inner_record, Span::test_data()));
        assert_eq!(*result.0.0, ["AMD64-unknown-elf-linux"]);
        assert_eq!(*result.1.0, ["foo"]);
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

        let result = value_to_fields(&Value::record(inner_record, Span::test_data()));
        let res: [String; 0] = [];
        assert_eq!(*result.1.0, res);
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

        let result = value_to_fields(&Value::record(inner_record, Span::test_data()));
        let res: [String; 0] = [];
        assert_eq!(*result.0.0, res);
    }

    #[test]
    fn values_to_fields_not_record() {
        let result = value_to_fields(&Value::string("foo", Span::test_data()));
        let res: [String; 0] = [];
        assert_eq!(*result.0.0, res);
        assert_eq!(*result.1.0, res);
    }

    #[test]
    fn parse_components_ok() {
        let components: Vec<_> = ["foo", "bar", "aaaa"]
            .into_iter()
            .map(|comp| Value::string(comp, Span::test_data()))
            .collect();

        let result = parse_components(&components);
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
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], "foo");
        assert_eq!(result[1], "bar");
        assert_eq!(result[2], "aaaa");
    }
}
