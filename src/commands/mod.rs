use anyhow::{Result, anyhow};
use std::process::{Command, Stdio};

#[derive(PartialEq, Eq)]
pub enum Perms {
    Root,
    User,
}

pub fn run_command_for_stdout<I, S>(args: I, perms: Perms, hide_stderr: bool) -> Result<String>
where
    S: Into<String>,
    I: IntoIterator<Item = S>,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect::<Vec<_>>();

    if args.is_empty() {
        return Err(anyhow!("cannot run an empty command"));
    }

    let args = Some("sudo".to_string())
        .filter(|_| perms == Perms::Root)
        .into_iter()
        .chain(args)
        .collect::<Vec<_>>();

    let (first_arg, remaining_args) = args.split_first().unwrap();

    let mut command = Command::new(first_arg);
    let output = command
        .args(remaining_args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(if !hide_stderr {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .output()?;

    if output.status.success() {
        Ok(String::from_utf8(output.stdout)?)
    } else {
        Err(anyhow!("command failed: {:?}", args.join(" ")))
    }
}

pub fn run_command<I, S>(args: I, perms: Perms) -> Result<()>
where
    S: Into<String>,
    I: IntoIterator<Item = S>,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect::<Vec<_>>();

    if args.is_empty() {
        return Err(anyhow!("cannot run an empty command"));
    }

    let args = Some("sudo".to_string())
        .filter(|_| perms == Perms::Root)
        .into_iter()
        .chain(args)
        .collect::<Vec<_>>();

    let (first_arg, remaining_args) = args.split_first().unwrap();

    let mut command = Command::new(first_arg);
    let status = command
        .args(remaining_args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("command failed: {:?}", args.join(" ")))
    }
}
