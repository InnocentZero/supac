use std::collections::HashMap;

use anyhow::anyhow;
use nu_protocol::{Record, Value};

use crate::parser::Engine;

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
        todo!()
    }

    fn remove(&self, config: &mut Record) -> anyhow::Result<()> {
        todo!()
    }

    fn clean_cache(&self, config: &Record) -> anyhow::Result<()> {
        todo!()
    }
}

fn values_to_pkgspec(record: &Record) -> HashMap<String, (Targets, Components)> {
    record
        .iter()
        .map(|(toolchain, value)| (toolchain.to_owned(), value_to_fields(value)))
        .collect()
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
}
