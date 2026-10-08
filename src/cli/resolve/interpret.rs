// SPDX-License-Identifier: Apache-2.0

//! `rez interpret` - execute rex DSL code and print shell output.

use std::collections::HashMap;

use clap::Args;

use foundation::errors::{Result, RezError};
use rex::rex::{Action, OutputStyle, PythonInterpreter, RexExecutor};
use rex::types::{create_shell, detect_shell, Shell, ShellType};

// =============================================================================
// Args
// =============================================================================

/// Execute Rex code and print the interpreted result.
#[derive(Args, Debug)]
pub struct InterpretArgs {
    /// File containing rex code to execute.
    #[arg(value_name = "FILE")]
    pub file: String,

    /// Output format: shell name (bash, cmd, powershell...), "dict", or "table".
    /// Defaults to the current shell.
    #[arg(short = 'f', long)]
    pub format: Option<String>,

    /// Interpret the code in an empty environment.
    #[arg(long)]
    pub no_env: bool,

    /// Environment variables to update rather than overwrite on first reference.
    /// Use "all" to treat all variables this way.
    #[arg(long = "pv", visible_alias = "parent-variables", num_args = 1..)]
    pub parent_vars: Vec<String>,
}

// =============================================================================
// Run
// =============================================================================

pub fn run(args: &InterpretArgs) -> Result<()> {
    // Read rex code from file
    let code = std::fs::read_to_string(&args.file)
        .map_err(|e| RezError::Config(format!("Cannot read file {:?}: {e}", args.file)))?;

    let fmt = args.format.as_deref();
    let parent_env: Option<HashMap<String, String>> = if args.no_env {
        Some(HashMap::new())
    } else {
        None
    };

    let actions = parse_rex_code(&code)?;

    // Choose interpreter based on format
    match fmt {
        Some("dict") | Some("table") => {
            let interp = PythonInterpreter::new();
            let mut exec = RexExecutor::new(interp, parent_env, false);
            exec.execute_actions(&actions);

            let env = exec.env().clone();
            let mut pairs: Vec<_> = env.iter().collect();
            pairs.sort_by_key(|(k, _)| (*k).clone());

            if fmt == Some("table") {
                for (k, v) in pairs {
                    println!("{k:<40} {v}");
                }
            } else {
                println!("{{");
                for (k, v) in pairs {
                    println!("  {k:?}: {v:?},");
                }
                println!("}}");
            }
        }
        _ => {
            // Shell interpreter
            let shell_type = match fmt {
                Some(name) => ShellType::from_name(name)
                    .ok_or_else(|| RezError::Config(format!("Unknown shell type: {name:?}")))?,
                None => detect_shell(),
            };
            let sh = create_shell(shell_type);
            let mut exec = RexExecutor::new(sh, parent_env, false);
            exec.execute_actions(&actions);

            let shell: &dyn Shell = exec.interpreter().as_ref();
            let output = Shell::get_output(shell, OutputStyle::File);
            println!("{output}");
        }
    }

    Ok(())
}

// =============================================================================
// Rex code parser
// =============================================================================

/// Parse a rex code string into a list of Actions.
///
/// Supports: setenv, unsetenv, prependenv, appendenv, alias, comment,
///           command, source, resetenv, error, info, stop.
fn parse_rex_code(code: &str) -> Result<Vec<Action>> {
    let mut actions = Vec::new();

    for line in code.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Parse function call: name(args...)
        let Some(paren) = line.find('(') else {
            continue;
        };
        let func = line[..paren].trim();
        let rest = line[paren + 1..].trim();
        let rest = rest.strip_suffix(')').unwrap_or(rest);

        let args = parse_args(rest);

        let action = match func {
            "setenv" if args.len() >= 2 => Action::Setenv {
                key: args[0].clone(),
                value: args[1].clone().into(),
            },
            "unsetenv" if !args.is_empty() => Action::Unsetenv {
                key: args[0].clone(),
            },
            "prependenv" | "prepend" if args.len() >= 2 => Action::Prependenv {
                key: args[0].clone(),
                value: args[1].clone().into(),
            },
            "appendenv" | "append" if args.len() >= 2 => Action::Appendenv {
                key: args[0].clone(),
                value: args[1].clone().into(),
            },
            "resetenv" if args.len() >= 2 => Action::Resetenv {
                key: args[0].clone(),
                value: args[1].clone().into(),
                friends: None,
            },
            "alias" if args.len() >= 2 => Action::Alias {
                name: args[0].clone(),
                cmd: args[1].clone(),
            },
            "info" => Action::Info {
                msg: args.first().cloned().unwrap_or_default().into(),
            },
            "error" => Action::Error {
                msg: args.first().cloned().unwrap_or_default().into(),
            },
            "command" if !args.is_empty() => Action::Command {
                cmd: args[0].clone(),
            },
            "source" if !args.is_empty() => Action::Source {
                path: args[0].clone().into(),
            },
            "comment" => Action::Comment {
                text: args.first().cloned().unwrap_or_default(),
            },
            "stop" => Action::Stop {
                msg: args.first().cloned().unwrap_or_default().into(),
            },
            _ => continue, // skip unknown functions
        };

        actions.push(action);
    }

    Ok(actions)
}

/// Parse comma-separated, possibly quoted arguments.
fn parse_args(input: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let mut quote_char = '"';

    for ch in input.chars() {
        if in_quote {
            if ch == quote_char {
                in_quote = false;
            } else {
                current.push(ch);
            }
        } else {
            match ch {
                '"' | '\'' => {
                    in_quote = true;
                    quote_char = ch;
                }
                ',' => {
                    let trimmed = current.trim().to_string();
                    if !trimmed.is_empty() {
                        args.push(trimmed);
                    }
                    current.clear();
                }
                _ => current.push(ch),
            }
        }
    }

    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        args.push(trimmed);
    }

    args
}
