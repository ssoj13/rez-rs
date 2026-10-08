// SPDX-License-Identifier: Apache-2.0

//! Built-in release hook plugins: amqp, emailer, command.
//!
//! When `config.release_hooks` contains "amqp", "emailer", or "command",
//! the corresponding built-in is run instead of invoking an external script.
//! Settings come from `config.plugins.release_hook.{amqp,emailer,command}`.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

#[cfg(feature = "amqp")]
use crate::amqp;
use crate::config::CONFIG;
use crate::errors::{Result, RezError};

// ---------------------------------------------------------------------------
// Built-in hook runners
// ---------------------------------------------------------------------------

/// Context passed to a built-in release hook.
pub struct ReleaseHookContext<'a> {
    pub event: &'a str,
    pub working_dir: &'a Path,
    pub pkg_name: &'a str,
    pub pkg_version: &'a str,
    pub install_path: Option<&'a Path>,
    pub message: Option<&'a str>,
    pub verbose: bool,
}

/// Run a built-in release hook by name.
/// Returns true if the hook name is a known built-in and it was run.
/// Returns false if the name is not a built-in (caller should run script/command).
pub fn run_builtin_hook(name: &str, ctx: &ReleaseHookContext<'_>) -> Result<bool> {
    match name {
        #[cfg(feature = "amqp")]
        "amqp" => {
            run_amqp_hook(
                ctx.event,
                ctx.pkg_name,
                ctx.pkg_version,
                ctx.install_path,
                ctx.verbose,
            )?;
            Ok(true)
        }
        "emailer" => {
            run_emailer_hook(
                ctx.event,
                ctx.pkg_name,
                ctx.pkg_version,
                ctx.install_path,
                ctx.message,
                ctx.verbose,
            )?;
            Ok(true)
        }
        "command" => {
            run_command_hook(
                ctx.event,
                ctx.working_dir,
                ctx.pkg_name,
                ctx.pkg_version,
                ctx.install_path,
                ctx.verbose,
            )?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn get_hook_settings(name: &str) -> HashMap<String, Value> {
    let mut defaults: HashMap<String, Value> = HashMap::new();
    match name {
        #[cfg(feature = "amqp")]
        "amqp" => {
            defaults.insert("host".into(), Value::String(String::new()));
            defaults.insert("exchange_name".into(), Value::String(String::new()));
            defaults.insert(
                "exchange_routing_key".into(),
                Value::String("REZ.PACKAGE.RELEASED".into()),
            );
            defaults.insert("message_delivery_mode".into(), Value::Number(1u32.into()));
        }
        "emailer" => {
            defaults.insert(
                "subject".into(),
                Value::String("Rez package released: {package.qualified_name}".into()),
            );
            defaults.insert(
                "body".into(),
                Value::String(
                    "Package {package.qualified_name} was released.\n\nPath: {release.path}".into(),
                ),
            );
            defaults.insert("smtp_host".into(), Value::String(String::new()));
            defaults.insert("smtp_port".into(), Value::Number(25i32.into()));
            defaults.insert("sender".into(), Value::String(String::new()));
            defaults.insert("recipients".into(), Value::String(String::new()));
        }
        "command" => {
            defaults.insert("print_commands".into(), Value::Bool(false));
            defaults.insert("print_output".into(), Value::Bool(false));
            defaults.insert("print_error".into(), Value::Bool(true));
            defaults.insert("cancel_on_error".into(), Value::Bool(false));
            defaults.insert("stop_on_error".into(), Value::Bool(false));
            defaults.insert("pre_release_commands".into(), Value::Array(vec![]));
            defaults.insert("post_release_commands".into(), Value::Array(vec![]));
        }
        _ => {}
    }
    if let Some(plugins) = CONFIG.plugins.get("release_hook") {
        if let Some(plugin) = plugins.get(name) {
            if let Some(obj) = plugin.as_object() {
                for (k, v) in obj {
                    defaults.insert(k.clone(), v.clone());
                }
            }
        }
    }
    defaults
}

#[cfg(feature = "amqp")]
fn run_amqp_hook(
    event: &str,
    pkg_name: &str,
    pkg_version: &str,
    install_path: Option<&Path>,
    verbose: bool,
) -> Result<()> {
    if event != "post_release" {
        return Ok(());
    }
    let settings = get_hook_settings("amqp");
    let host = settings
        .get("host")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if host.is_empty() {
        if verbose {
            eprintln!("AMQP release hook: host not configured, skipping");
        }
        return Ok(());
    }
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".into());
    let qualified_name = format!("{}-{}", pkg_name, pkg_version);
    let uri = install_path
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| qualified_name.clone());
    let mut data = serde_json::Map::new();
    let mut pkg_obj = serde_json::Map::new();
    pkg_obj.insert("name".into(), Value::String(pkg_name.to_string()));
    pkg_obj.insert("version".into(), Value::String(pkg_version.to_string()));
    pkg_obj.insert(
        "qualified_name".into(),
        Value::String(qualified_name.clone()),
    );
    pkg_obj.insert("uri".into(), Value::String(uri));
    pkg_obj.insert("user".into(), Value::String(user));
    pkg_obj.insert("handle".into(), Value::Object(serde_json::Map::new()));
    data.insert("package".into(), Value::Object(pkg_obj));
    data.insert("variants".into(), Value::Array(vec![]));
    if let Some(attrs) = settings
        .get("message_attributes")
        .and_then(|v| v.as_object())
    {
        for (k, v) in attrs {
            data.insert(k.clone(), v.clone());
        }
    }
    let routing_key = settings
        .get("exchange_routing_key")
        .and_then(|v| v.as_str())
        .unwrap_or("REZ.PACKAGE.RELEASED")
        .to_string();
    let amqp_map: HashMap<String, Value> = settings
        .iter()
        .filter(|(k, _)| !["host", "message_attributes"].contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if verbose {
        eprintln!("Publishing AMQP message on {}...", routing_key);
    }
    amqp::publish_message(host, &amqp_map, &routing_key, &Value::Object(data))?;
    Ok(())
}

fn run_emailer_hook(
    event: &str,
    pkg_name: &str,
    pkg_version: &str,
    install_path: Option<&Path>,
    message: Option<&str>,
    verbose: bool,
) -> Result<()> {
    if event != "post_release" {
        return Ok(());
    }
    let _ = message;
    let settings = get_hook_settings("emailer");
    let recipients = settings
        .get("recipients")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let smtp_host = settings
        .get("smtp_host")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if recipients.is_empty() {
        if verbose {
            eprintln!("Emailer release hook: no recipients configured, skipping");
        }
        return Ok(());
    }
    if smtp_host.is_empty() {
        eprintln!("Warning: emailer release hook: SMTP host not specified, skipping");
        return Ok(());
    }
    let qualified_name = format!("{}-{}", pkg_name, pkg_version);
    let subject = settings
        .get("subject")
        .and_then(|v| v.as_str())
        .unwrap_or("Rez package released")
        .replace("{package.qualified_name}", &qualified_name);
    let body_tpl = settings
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("Package released.");
    let body = body_tpl
        .replace("{package.qualified_name}", &qualified_name)
        .replace(
            "{release.path}",
            &install_path
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        );
    let sender = settings
        .get("sender")
        .and_then(|v| v.as_str())
        .unwrap_or("rez@local")
        .trim();
    let smtp_port = settings
        .get("smtp_port")
        .and_then(|v| v.as_i64())
        .unwrap_or(25) as u16;
    if verbose {
        eprintln!("Sending release email to: {}", recipients);
    }
    send_email_smtp(smtp_host, smtp_port, sender, recipients, &subject, &body)?;
    Ok(())
}

fn send_email_smtp(
    host: &str,
    port: u16,
    from: &str,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<()> {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    let mut stream = TcpStream::connect(format!("{}:{}", host, port))
        .map_err(|e| RezError::Release(format!("SMTP connect failed: {}", e)))?;
    let mut buf = [0u8; 512];
    let _ = stream.read(&mut buf);
    let to_addrs: Vec<&str> = to
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if to_addrs.is_empty() {
        return Ok(());
    }
    let from_addr = if from.contains('<') {
        from.to_string()
    } else {
        format!("<{}>", from)
    };

    fn write_smtp(s: &mut TcpStream, cmd: &str, buf: &mut [u8]) -> Result<()> {
        s.write_all(format!("{}\r\n", cmd).as_bytes())
            .map_err(|e| RezError::Release(format!("SMTP write failed: {}", e)))?;
        let _ = s.read(buf);
        Ok(())
    }
    write_smtp(&mut stream, "EHLO rez", &mut buf)?;
    write_smtp(&mut stream, &format!("MAIL FROM:{}", from_addr), &mut buf)?;
    for addr in &to_addrs {
        let a = if addr.contains('<') {
            addr.to_string()
        } else {
            format!("<{}>", addr)
        };
        write_smtp(&mut stream, &format!("RCPT TO:{}", a), &mut buf)?;
    }
    write_smtp(&mut stream, "DATA", &mut buf)?;
    let msg = format!(
        "From: {}\r\nTo: {}\r\nSubject: {}\r\n\r\n{}\r\n.",
        from,
        to_addrs.join(", "),
        subject,
        body
    );
    stream
        .write_all(msg.as_bytes())
        .map_err(|e| RezError::Release(format!("SMTP write failed: {}", e)))?;
    let _ = stream.read(&mut buf);
    eprintln!("Email(s) sent to {}.", to_addrs.join(", "));
    Ok(())
}

fn which_in_path(name: &str) -> Option<std::path::PathBuf> {
    let path_var = std::env::var("PATH").ok()?;
    let exe_ext = std::env::consts::EXE_SUFFIX;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(format!("{}{}", name, exe_ext));
        if candidate.is_file() {
            return Some(candidate);
        }
        let candidate_no_ext = dir.join(name);
        if candidate_no_ext.is_file() {
            return Some(candidate_no_ext);
        }
    }
    None
}

fn run_command_hook(
    event: &str,
    working_dir: &Path,
    pkg_name: &str,
    pkg_version: &str,
    install_path: Option<&Path>,
    verbose: bool,
) -> Result<()> {
    let settings = get_hook_settings("command");
    let commands_key = match event {
        "pre_release" => "pre_release_commands",
        "post_release" => "post_release_commands",
        _ => return Ok(()),
    };
    let commands = settings
        .get(commands_key)
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let cancel_on_error = settings
        .get("cancel_on_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let print_commands = settings
        .get("print_commands")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let print_output = settings
        .get("print_output")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let install_path_str = install_path
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let qualified_name = format!("{}-{}", pkg_name, pkg_version);

    for cmd_conf in commands {
        let cmd_obj = cmd_conf.as_object().ok_or_else(|| {
            RezError::Release("command hook: each entry must be an object".into())
        })?;
        let program = cmd_obj
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RezError::Release("command hook: missing 'command' field".into()))?;
        let args_raw = cmd_obj.get("args");
        let args: Vec<String> = match args_raw {
            Some(Value::String(s)) => s.split_whitespace().map(|x| x.to_string()).collect(),
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
            _ => vec![],
        };
        let args: Vec<String> = args
            .iter()
            .map(|a| {
                a.replace("{package.name}", pkg_name)
                    .replace("{package.version}", pkg_version)
                    .replace("{package.qualified_name}", &qualified_name)
                    .replace("{release.path}", &install_path_str)
            })
            .collect();

        let prog_path = if program.contains('/') || program.contains('\\') {
            working_dir.join(program)
        } else if let Some(p) = which_in_path(program) {
            p
        } else {
            return Err(RezError::Release(format!(
                "command hook: '{}' not found in PATH",
                program
            )));
        };

        if print_commands || verbose {
            eprintln!(
                "Running command: {} {}",
                prog_path.display(),
                args.join(" ")
            );
        }
        let output = Command::new(&prog_path)
            .args(&args)
            .current_dir(working_dir)
            .output()
            .map_err(|e| RezError::Release(format!("command hook failed: {}", e)))?;
        if !output.status.success() {
            let err_msg = String::from_utf8_lossy(&output.stderr);
            if print_output {
                eprintln!("{}", err_msg);
            }
            if cancel_on_error {
                return Err(RezError::Release(format!(
                    "command hook '{}' failed: {}",
                    program,
                    err_msg.trim()
                )));
            }
        } else if print_output {
            let out = String::from_utf8_lossy(&output.stdout);
            if !out.is_empty() {
                eprintln!("{}", out);
            }
        }
    }
    Ok(())
}
