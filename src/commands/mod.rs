use std::process::{Command, Stdio};

use anyhow::{Result, anyhow};
use inquire::Confirm;

use crate::{function, mod_err};

#[derive(PartialEq, Eq, Debug, Copy, Clone)]
pub enum Perms {
    Root,
    User,
}

pub fn run_command_for_stdout<I, S>(args: I, perms: Perms, hide_stderr: bool) -> Result<String>
where
    S: Into<String>,
    I: IntoIterator<Item = S>,
{
    let args = get_command(args, perms)?;

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
        Err(mod_err!("command failed: {:?}", args.join(" ")))
    }
}

pub fn run_command<I, S>(args: I, perms: Perms) -> Result<()>
where
    S: Into<String>,
    I: IntoIterator<Item = S>,
{
    let args = get_command(args, perms)?;

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
        Err(mod_err!("command failed: {:?}", args.join(" ")))
    }
}

pub fn dry_run_command<I, S>(args: I, perms: Perms) -> Result<()>
where
    S: Into<String>,
    I: IntoIterator<Item = S>,
{
    let command = get_command(args, perms)?;
    let command_str = "DRY RUN COMMAND> ".to_owned() + command.join(" ").as_str();

    #[allow(clippy::print_stderr)]
    {
        eprintln!("{command_str}");
    }
    Ok(())
}

pub fn confirmation_prompt<P, I, S>(prompt: P, items: I) -> Result<bool>
where
    P: AsRef<str>,
    S: AsRef<str>,
    I: IntoIterator<Item = S>,
{
    let answer = Confirm::new(prompt.as_ref())
        .with_default(true)
        .with_help_message(
            items
                .into_iter()
                .fold(" ".to_owned(), |acc, elem| acc + elem.as_ref() + " ")
                .as_str(),
        )
        .prompt();

    answer.map_err(|_| mod_err!("Failed to retrieve answer"))
}

fn get_command<I, S>(args: I, perms: Perms) -> Result<Vec<String>>
where
    S: Into<String>,
    I: IntoIterator<Item = S>,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();

    if args.is_empty() {
        return Err(mod_err!("cannot run an empty command"));
    }

    let command = Some("sudo".to_string())
        .filter(|_| perms == Perms::Root)
        .into_iter()
        .chain(args)
        .collect();

    Ok(command)
}
