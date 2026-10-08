//! `rez python` forwards to the same executable's native RustPython CLI.
use std::ffi::OsString;
use std::process::{Command, ExitCode};

use clap::Args;

use foundation::errors::{Result, RezError};

#[derive(Args, Debug)]
#[command(disable_help_flag = true)]
pub struct PythonArgs {
    /// Python interpreter options, script/module and arguments.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub arguments: Vec<OsString>,
}

/// Locate the Python subcommand without parsing its interpreter arguments.
pub fn command_index(command: &clap::Command, argv: &[OsString]) -> Option<usize> {
    argv.iter().enumerate().skip(1).find_map(|(index, arg)| {
        if arg != "python" {
            return None;
        }
        command
            .clone()
            .try_get_matches_from(&argv[..=index])
            .ok()
            .filter(|matches| matches.subcommand_name() == Some("python"))
            .map(|_| index)
    })
}

/// Recognize interpreter invocations made through Python's real sys.executable.
pub fn is_reentry(command: &clap::Command, argv: &[OsString]) -> bool {
    if argv
        .first()
        .is_some_and(|path| python_runtime::is_python_executable(path))
    {
        return true;
    }
    // Valid Rez commands always retain their own options and positional arguments.
    if command.clone().try_get_matches_from(argv).is_ok() {
        return false;
    }
    let Some(selector) = argv.iter().skip(1).find(|arg| {
        let value = arg.to_string_lossy();
        value != "--verbose"
            && !(value.starts_with('-') && value.len() > 1 && value[1..].chars().all(|c| c == 'v'))
    }) else {
        return false;
    };
    let value = selector.to_string_lossy();
    if command
        .get_subcommands()
        .any(|child| child.get_name() == value)
    {
        return false;
    }
    let python_option = matches!(
        value.as_ref(),
        "-" | "-c"
            | "-m"
            | "-I"
            | "-E"
            | "-S"
            | "-s"
            | "-u"
            | "-B"
            | "-O"
            | "-OO"
            | "-P"
            | "-q"
            | "-i"
            | "-b"
            | "-bb"
            | "-d"
            | "-R"
    ) || ["-c", "-m", "-W", "-X"]
        .iter()
        .any(|prefix| value.starts_with(prefix));
    let path = std::path::Path::new(selector);
    python_option
        || path.is_file()
        || path.is_dir()
        || matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("py" | "pyc" | "pyz")
        )
}

/// Use upstream's complete option parser and execution lifecycle in a fresh process.
///
/// The public RustPython CLI reads process argv, so forwarding preserves all native
/// options without implementing a second parser. Stdio and environment are inherited;
/// the dedicated executable name distinguishes Python options from Rez options.
pub fn run(args: &PythonArgs, script: Option<tempfile::TempPath>) -> Result<ExitCode> {
    let executable = python_runtime::executable()?
        .ok_or_else(|| RezError::Python("Python CLI entry point is not registered".into()))?;
    let mut command = Command::new(executable);
    if let Some(script) = &script {
        command.arg(script.as_os_str());
    }
    let status = command
        .args(&args.arguments)
        .status()
        .map_err(|error| RezError::Python(format!("start Python interpreter: {error}")))?;
    // Windows codes above 255 terminate immediately in the shared converter.
    drop(script);
    Ok(rustpython_host_env::os::exit_code(
        status.code().unwrap_or(1) as u32,
    ))
}
