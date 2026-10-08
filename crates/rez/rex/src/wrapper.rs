// SPDX-License-Identifier: Apache-2.0

//! Tool wrapper generation for suite contexts.
//!
//! Ported from Python rez wrapper.py.

use std::fs;
use std::path::{Path, PathBuf};

use crate::shell::rex::{Action, OutputStyle, RexValue};
use crate::shell::types::ShellType;
use foundation::errors::{Result, RezError};
use model::platform::Platform;

// =============================================================================
// Completion Scripts
// =============================================================================

/// Returns bash completion script for rez commands.
const BASH_COMPLETION: &str = r#"
_rez_complete_fn()
{
    local cur prev opts
    COMPREPLY=()
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD-1]}"
    
    # Main rez commands
    if [ $COMP_CWORD -eq 1 ]; then
        opts="env build test config search view help-pkg status bind suite bundle cp mv rm pkg-cache context interpret complete python pip release depends diff plugins selftest forward memcache benchmark yaml2py pkg-ignore"
        COMPREPLY=( $(compgen -W "${opts}" -- ${cur}) )
        return 0
    fi
}

complete -F _rez_complete_fn rez
"#;

/// Returns zsh completion script for rez commands.
const ZSH_COMPLETION: &str = r#"
#compdef rez

_rez() {
    local -a commands
    commands=(
        'env:Spawn shell with resolved context'
        'build:Build packages'
        'test:Run package tests'
        'config:Query config'
        'search:Search packages'
        'view:View package info'
        'help-pkg:Package help'
        'status:System status'
        'bind:Bind system packages'
        'suite:Manage suites'
        'bundle:Bundle contexts'
        'cp:Copy packages'
        'mv:Move packages'
        'rm:Remove packages'
        'pkg-cache:Manage cache'
        'context:Inspect .rxt'
        'interpret:Rex commands'
        'complete:Shell completions'
        'python:Python REPL'
        'pip:Install pip packages'
        'release:Build and deploy'
        'depends:Reverse dependencies'
        'diff:Compare package versions'
        'plugins:List package plugins'
        'selftest:Run cargo test'
        'forward:YAML forwarding script'
        'memcache:Manage memcache servers'
        'benchmark:Resolve benchmarking'
        'yaml2py:Convert package.yaml to package.py'
        'pkg-ignore:Ignore/unignore packages'
    )
    
    _describe 'rez commands' commands
}

_rez "$@"
"#;

/// Returns PowerShell completion script for rez commands.
const PWSH_COMPLETION: &str = r#"
Register-ArgumentCompleter -CommandName rez -ScriptBlock {
    param($commandName, $wordToComplete, $commandAst, $fakeBoundParameters)
    
    $commands = @(
        'env', 'build', 'test', 'config', 'search', 'view', 'help-pkg', 'status',
        'bind', 'suite', 'bundle', 'cp', 'mv', 'rm', 'pkg-cache', 'context',
        'interpret', 'complete', 'python', 'pip', 'release', 'depends', 'diff',
        'plugins', 'selftest', 'forward', 'memcache', 'benchmark', 'yaml2py', 'pkg-ignore'
    )
    
    $commands | Where-Object { $_ -like "$wordToComplete*" } | ForEach-Object {
        [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_)
    }
}
"#;

// =============================================================================
// Wrapper struct
// =============================================================================

/// A tool wrapper in a suite.
///
/// Wrappers reside in the `./bin` directory of a suite. When executed, they
/// load the associated resolved context and run the tool within it.
#[derive(Debug, Clone)]
pub struct Wrapper {
    /// Root path of the suite containing this wrapper.
    pub suite_path: PathBuf,
    /// Name of the resolved context (without .rxt extension).
    pub context_name: String,
    /// Name of the underlying tool to invoke.
    pub tool_name: String,
    /// Optional prefix for the executable name.
    pub prefix: Option<String>,
    /// Optional suffix for the executable name.
    pub suffix: Option<String>,
}

impl Wrapper {
    /// Create a new wrapper descriptor.
    pub fn new(
        suite_path: impl Into<PathBuf>,
        context_name: impl Into<String>,
        tool_name: impl Into<String>,
    ) -> Self {
        Self {
            suite_path: suite_path.into(),
            context_name: context_name.into(),
            tool_name: tool_name.into(),
            prefix: None,
            suffix: None,
        }
    }

    /// Set prefix for executable name.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = Some(prefix.into());
        self
    }

    /// Set suffix for executable name.
    pub fn with_suffix(mut self, suffix: impl Into<String>) -> Self {
        self.suffix = Some(suffix.into());
        self
    }

    /// The executable name with optional prefix and suffix applied.
    ///
    /// E.g. tool "maya" with prefix "suite_" -> "suite_maya".
    pub fn executable_name(&self) -> String {
        let mut name = String::new();
        if let Some(ref pfx) = self.prefix {
            name.push_str(pfx);
        }
        name.push_str(&self.tool_name);
        if let Some(ref sfx) = self.suffix {
            name.push_str(sfx);
        }
        name
    }

    /// Path to the resolved context (.rxt) file.
    pub fn context_path(&self) -> PathBuf {
        self.suite_path
            .join("contexts")
            .join(format!("{}.rxt", self.context_name))
    }

    /// Path to the wrapper script in the suite's bin directory.
    pub fn wrapper_path(&self, shell: ShellType) -> PathBuf {
        let ext = shell.file_ext();
        let name = self.executable_name();
        if Platform::current() == Platform::Windows && shell == ShellType::Cmd {
            self.suite_path.join("bin").join(format!("{name}.cmd"))
        } else {
            self.suite_path.join("bin").join(format!("{name}.{ext}"))
        }
    }

    /// Print a human-readable summary of this wrapper.
    pub fn print_about(&self) {
        println!("Tool:     {}", self.tool_name);
        println!("Exec:     {}", self.executable_name());
        println!("Suite:    {}", self.suite_path.display());
        println!(
            "Context:  {} ({})",
            self.context_name,
            self.context_path().display()
        );
    }
}

// =============================================================================
// WrapperScript enum
// =============================================================================

/// Validate a variable name before embedding it in shell source.
fn is_environment_key(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn contains_script_control(value: &str) -> bool {
    value.chars().any(|ch| matches!(ch, '\0' | '\r' | '\n'))
}

/// Quote a literal for POSIX shells.
fn posix_quote(value: &str) -> String {
    ShellType::Bash.render_value(&RexValue::literal(value), OutputStyle::File, true)
}

/// Quote a literal for PowerShell single-quoted strings.
fn powershell_quote(value: &str) -> String {
    ShellType::PowerShell.render_value(&RexValue::literal(value), OutputStyle::File, true)
}

/// Quote an argument for cmd.exe and the Windows C runtime.
fn cmd_quote(value: &str) -> String {
    let trailing_backslashes = value.chars().rev().take_while(|ch| *ch == '\\').count();
    format!(
        "\"{}{}\"",
        cmd_escape(value),
        "\\".repeat(trailing_backslashes)
    )
}

/// Escape percent expansion in a batch file.
fn cmd_escape(value: &str) -> String {
    value.replace('%', "%%")
}

/// Quote a literal for Csh/Tcsh without allowing variable or history expansion.
fn csh_quote(value: &str) -> String {
    ShellType::Csh.render_value(&RexValue::literal(value), OutputStyle::File, true)
}

/// Shell-specific wrapper script content.
#[derive(Debug, Clone)]
pub enum WrapperScript {
    /// Bash/sh script content.
    Bash(String),
    /// Windows cmd.exe batch script content.
    Cmd(String),
    /// PowerShell script content.
    PowerShell(String),
    /// Csh/Tcsh script content.
    Csh(String),
}

impl WrapperScript {
    /// Generate a wrapper script that sources a context and runs a tool.
    pub fn generate(context_path: &Path, tool_name: &str, shell: ShellType) -> Self {
        let source = shell.render_action(
            &Action::Source {
                path: RexValue::literal(context_path.to_string_lossy().into_owned()),
            },
            OutputStyle::File,
        );
        let tool = if shell == ShellType::Cmd {
            cmd_quote(tool_name)
        } else {
            shell.join_command(
                &[tool_name.to_owned()],
                false,
                (shell == ShellType::PowerShell).then_some("$args"),
            )
        };
        match shell {
            ShellType::Cmd => Self::Cmd(format!(
                "@echo off\r\nrem Rez wrapper script - auto-generated\r\n{source}\r\n{tool} %*\r\n"
            )),
            ShellType::PowerShell => Self::PowerShell(format!(
                "# Rez wrapper script - auto-generated\n{source}\n{tool}\n{}\n",
                shell.exit_command()
            )),
            ShellType::Csh | ShellType::Tcsh => Self::Csh(format!(
                "#!/bin/{} -f\n# Rez wrapper script - auto-generated\n{source}\nexec {tool} $argv:q\n",
                if shell == ShellType::Tcsh {
                    "tcsh"
                } else {
                    "csh"
                }
            )),
            _ => Self::Bash(format!(
                "#!/bin/bash\n# Rez wrapper script - auto-generated\n{source}\nexec {tool} \"$@\"\n"
            )),
        }
    }

    /// Generate a shell-safe forwarding script for a program and its fixed arguments.
    ///
    /// Environment values and the working directory are embedded as literals.
    /// Runtime arguments are forwarded unchanged using the selected shell's native
    /// argument vector syntax.
    pub fn generate_forward(
        program: &str,
        args: &[String],
        env_vars: &std::collections::HashMap<String, String>,
        working_dir: Option<&Path>,
        shell: ShellType,
    ) -> Result<Self> {
        let mut env_vars: Vec<_> = env_vars.iter().collect();
        env_vars.sort_by_key(|(left, _)| *left);
        for (key, value) in &env_vars {
            if !is_environment_key(key) || contains_script_control(value) {
                return Err(RezError::Config(format!(
                    "Cannot generate forwarding script with invalid environment entry {key:?}"
                )));
            }
        }
        if contains_script_control(program)
            || args.iter().any(|arg| contains_script_control(arg))
            || working_dir.is_some_and(|path| contains_script_control(&path.to_string_lossy()))
        {
            return Err(RezError::Config(
                "Cannot generate forwarding script with a control character in a command path or argument".into(),
            ));
        }
        if shell == ShellType::Cmd
            && (program.contains('"')
                || args.iter().any(|arg| arg.contains('"'))
                || env_vars.iter().any(|(_, value)| value.contains('"'))
                || working_dir.is_some_and(|path| path.to_string_lossy().contains('"')))
        {
            return Err(RezError::Config(
                "Cannot generate a cmd script with a double quote in a command path, argument, or environment value".into(),
            ));
        }

        let script = match shell {
            ShellType::Cmd => {
                let mut lines = vec![
                    "@echo off".to_string(),
                    "setlocal DisableDelayedExpansion".to_string(),
                    "rem Rez forwarding script - auto-generated".to_string(),
                ];
                if let Some(path) = working_dir {
                    lines.push(format!(
                        "cd /d {} || exit /b 1",
                        cmd_quote(&path.to_string_lossy())
                    ));
                }
                for (key, value) in &env_vars {
                    lines.push(format!("set \"{key}={}\"", cmd_escape(value)));
                }
                let mut command = vec![cmd_quote(program)];
                command.extend(args.iter().map(|arg| cmd_quote(arg)));
                lines.push(format!("{} %*", command.join(" ")));
                lines.push(shell.exit_command().into());
                Self::Cmd(format!("{}\r\n", lines.join("\r\n")))
            }
            ShellType::PowerShell => {
                let mut lines = vec!["# Rez forwarding script - auto-generated".to_string()];
                if let Some(path) = working_dir {
                    lines.push(format!(
                        "Set-Location -LiteralPath {} -ErrorAction Stop",
                        powershell_quote(&path.to_string_lossy())
                    ));
                }
                for (key, value) in &env_vars {
                    lines.push(format!("$env:{key} = {}", powershell_quote(value)));
                }
                let mut command = vec![program.to_owned()];
                command.extend_from_slice(args);
                lines.push(shell.join_command(&command, false, Some("$args")));
                lines.push(shell.exit_command().into());
                Self::PowerShell(format!("{}\n", lines.join("\n")))
            }
            ShellType::Csh | ShellType::Tcsh => {
                let interpreter = if shell == ShellType::Tcsh {
                    "tcsh"
                } else {
                    "csh"
                };
                let mut lines = vec![
                    format!("#!/bin/{interpreter} -f"),
                    "# Rez forwarding script - auto-generated".into(),
                ];
                if let Some(path) = working_dir {
                    lines.push(format!("cd {}", csh_quote(&path.to_string_lossy())));
                    lines.push("if ($status != 0) exit $status".into());
                }
                for (key, value) in &env_vars {
                    lines.push(format!("setenv {key} {}", csh_quote(value)));
                }
                let mut command = vec![csh_quote(program)];
                command.extend(args.iter().map(|arg| csh_quote(arg)));
                command.push("$argv:q".into());
                lines.push(format!("exec {}", command.join(" ")));
                Self::Csh(format!("{}\n", lines.join("\n")))
            }
            ShellType::Bash | ShellType::Sh | ShellType::Gitbash | ShellType::Zsh => {
                let mut lines = vec![
                    "#!/bin/bash".to_string(),
                    "# Rez forwarding script - auto-generated".into(),
                ];
                if let Some(path) = working_dir {
                    lines.push(format!(
                        "cd -- {} || exit $?",
                        posix_quote(&path.to_string_lossy())
                    ));
                }
                for (key, value) in &env_vars {
                    lines.push(format!("export {key}={}", posix_quote(value)));
                }
                let mut command = vec![posix_quote(program)];
                command.extend(args.iter().map(|arg| posix_quote(arg)));
                command.push("\"$@\"".into());
                lines.push(format!("exec {}", command.join(" ")));
                Self::Bash(format!("{}\n", lines.join("\n")))
            }
        };
        Ok(script)
    }

    /// The script content as a string reference.
    pub fn content(&self) -> &str {
        match self {
            Self::Bash(s) | Self::Cmd(s) | Self::PowerShell(s) | Self::Csh(s) => s,
        }
    }

    /// File extension for this script type.
    pub fn file_ext(&self) -> &'static str {
        match self {
            Self::Bash(_) => "sh",
            Self::Cmd(_) => "cmd",
            Self::PowerShell(_) => "ps1",
            Self::Csh(_) => "csh",
        }
    }

    /// Write the script to a file, setting executable permissions on Unix.
    pub fn write_to(&self, path: &Path) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                RezError::System(format!(
                    "Failed to create directory {}: {e}",
                    parent.display()
                ))
            })?;
        }

        fs::write(path, self.content()).map_err(|e| {
            RezError::System(format!(
                "Failed to write wrapper script {}: {e}",
                path.display()
            ))
        })?;

        // Set executable permission on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o755);
            fs::set_permissions(path, perms).map_err(|e| {
                RezError::System(format!(
                    "Failed to set permissions on {}: {e}",
                    path.display()
                ))
            })?;
        }

        Ok(())
    }
}

// =============================================================================
// Completion binding
// =============================================================================

/// Returns shell-specific completion setup code for interactive rez shells.
///
/// This function returns the code that should be sourced when spawning an
/// interactive rez shell to enable tab-completion of rez commands.
pub fn bind_interactive_rez(shell: ShellType) -> String {
    match shell {
        ShellType::Bash | ShellType::Gitbash | ShellType::Sh => BASH_COMPLETION.to_string(),
        ShellType::Zsh => ZSH_COMPLETION.to_string(),
        ShellType::PowerShell => PWSH_COMPLETION.to_string(),
        ShellType::Cmd => String::new(), // No completion for cmd
        _ => String::new(),
    }
}

/// Returns completion script content for a specific shell.
///
/// Alias for `bind_interactive_rez()` for API compatibility.
pub fn get_completion_script(shell: ShellType) -> String {
    bind_interactive_rez(shell)
}

// =============================================================================
// Factory functions
// =============================================================================

/// Create a wrapper script for a tool in a suite.
///
/// Generates a shell script that sources the resolved context and executes
/// the tool. Returns the path to the created wrapper file.
pub fn create_wrapper(
    bin_dir: &Path,
    context_path: &Path,
    tool_name: &str,
    shell: ShellType,
) -> Result<PathBuf> {
    let script = WrapperScript::generate(context_path, tool_name, shell);
    let ext = script.file_ext();

    let filename = if Platform::current() == Platform::Windows && shell == ShellType::Cmd {
        format!("{tool_name}.cmd")
    } else {
        format!("{tool_name}.{ext}")
    };

    let path = bin_dir.join(&filename);
    script.write_to(&path)?;
    Ok(path)
}

/// Options for creating a forwarding script.
#[derive(Debug, Clone, Copy)]
pub struct ForwardingScriptOptions<'a> {
    pub bin_dir: &'a Path,
    pub tool_name: &'a str,
    pub program: &'a str,
    pub args: &'a [String],
    pub env_vars: &'a std::collections::HashMap<String, String>,
    pub working_dir: Option<&'a Path>,
    pub shell: ShellType,
    pub extensionless_unix: bool,
}

/// Create a forwarding script that delegates to another command.
///
/// Used for simple aliases. Returns path to the created script file.
pub fn create_forwarding_script(options: ForwardingScriptOptions<'_>) -> Result<PathBuf> {
    let ForwardingScriptOptions {
        bin_dir,
        tool_name,
        program,
        args,
        env_vars,
        working_dir,
        shell,
        extensionless_unix,
    } = options;

    let script = WrapperScript::generate_forward(program, args, env_vars, working_dir, shell)?;
    let ext = script.file_ext();

    let filename = if Platform::current() == Platform::Windows && shell == ShellType::Cmd {
        format!("{tool_name}.cmd")
    } else if Platform::current() != Platform::Windows && extensionless_unix {
        tool_name.to_string()
    } else {
        format!("{tool_name}.{ext}")
    };

    let path = bin_dir.join(&filename);
    script.write_to(&path)?;
    Ok(path)
}

/// Get the wrapper template string for a given shell type.
pub fn get_wrapper_template(shell: ShellType) -> &'static str {
    match shell {
        ShellType::Cmd => {
            "@echo off\r\n\
             rem Rez wrapper script - auto-generated\r\n\
             call \"{context}\"\r\n\
             {tool} %*\r\n"
        }
        ShellType::PowerShell => {
            "# Rez wrapper script - auto-generated\n\
             . \"{context}\"\n\
             & {tool} @args\n"
        }
        ShellType::Csh => {
            "#!/bin/csh -f\n\\
             # Rez wrapper script - auto-generated\n\\
             source '{context}'\n\\
             exec '{tool}' $argv:q\n"
        }
        ShellType::Tcsh => {
            "#!/bin/tcsh -f\n\\
             # Rez wrapper script - auto-generated\n\\
             source '{context}'\n\\
             exec '{tool}' $argv:q\n"
        }
        _ => {
            "#!/bin/bash\n\\
             # Rez wrapper script - auto-generated\n\\
             source \"{context}\"\n\\
             exec {tool} \"$@\"\n"
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- Wrapper struct --

    #[test]
    fn test_wrapper_new() {
        let w = Wrapper::new("/tmp/suite", "myctx", "maya");
        assert_eq!(w.suite_path, PathBuf::from("/tmp/suite"));
        assert_eq!(w.context_name, "myctx");
        assert_eq!(w.tool_name, "maya");
        assert!(w.prefix.is_none());
        assert!(w.suffix.is_none());
    }

    #[test]
    fn test_wrapper_executable_name_plain() {
        let w = Wrapper::new("/tmp/suite", "ctx", "houdini");
        assert_eq!(w.executable_name(), "houdini");
    }

    #[test]
    fn test_wrapper_executable_name_prefix() {
        let w = Wrapper::new("/tmp/suite", "ctx", "maya").with_prefix("suite_");
        assert_eq!(w.executable_name(), "suite_maya");
    }

    #[test]
    fn test_wrapper_executable_name_suffix() {
        let w = Wrapper::new("/tmp/suite", "ctx", "maya").with_suffix("_v2");
        assert_eq!(w.executable_name(), "maya_v2");
    }

    #[test]
    fn test_wrapper_executable_name_both() {
        let w = Wrapper::new("/tmp/suite", "ctx", "maya")
            .with_prefix("prod_")
            .with_suffix("_v3");
        assert_eq!(w.executable_name(), "prod_maya_v3");
    }

    #[test]
    fn test_wrapper_context_path() {
        let w = Wrapper::new("/tmp/suite", "myctx", "tool");
        let expected = PathBuf::from("/tmp/suite/contexts/myctx.rxt");
        assert_eq!(w.context_path(), expected);
    }

    #[test]
    fn test_wrapper_path_bash() {
        let w = Wrapper::new("/tmp/suite", "ctx", "maya");
        let p = w.wrapper_path(ShellType::Bash);
        assert_eq!(p, PathBuf::from("/tmp/suite/bin/maya.sh"));
    }

    #[test]
    fn test_wrapper_path_powershell() {
        let w = Wrapper::new("/tmp/suite", "ctx", "maya");
        let p = w.wrapper_path(ShellType::PowerShell);
        assert_eq!(p, PathBuf::from("/tmp/suite/bin/maya.ps1"));
    }

    // -- WrapperScript generation --

    #[test]
    fn test_generate_bash_script() {
        let ctx = PathBuf::from("/tmp/suite/contexts/main.rxt");
        let script = WrapperScript::generate(&ctx, "maya", ShellType::Bash);
        let content = script.content();
        assert!(content.starts_with("#!/bin/bash"));
        assert!(content.contains(". '/tmp/suite/contexts/main.rxt'"));
        assert!(content.contains("exec 'maya' \"$@\""));
        assert_eq!(script.file_ext(), "sh");
    }

    #[test]
    fn test_generate_cmd_script() {
        let ctx = PathBuf::from("C:\\suites\\main\\contexts\\dev.rxt");
        let script = WrapperScript::generate(&ctx, "maya", ShellType::Cmd);
        let content = script.content();
        assert!(content.starts_with("@echo off"));
        assert!(
            content.contains("=C:\\suites\\main\\contexts\\dev.rxt")
                && content.contains("call \"%%REZ_RS_SOURCE_")
        );
        assert!(content.contains("\"maya\" %*"));
        assert_eq!(script.file_ext(), "cmd");
    }

    #[test]
    fn test_generate_powershell_script() {
        let ctx = PathBuf::from("/opt/suites/ctx.rxt");
        let script = WrapperScript::generate(&ctx, "houdini", ShellType::PowerShell);
        let content = script.content();
        assert!(content.contains(". '/opt/suites/ctx.rxt'"));
        let invocation =
            ShellType::PowerShell.join_command(&["houdini".into()], false, Some("$args"));
        assert!(content.contains(&invocation));
        assert!(content.ends_with(&format!("{}\n", ShellType::PowerShell.exit_command())));
        assert_eq!(script.file_ext(), "ps1");
    }

    #[test]
    fn test_generate_csh_and_tcsh_scripts() {
        let ctx = PathBuf::from("/tmp/suite/contexts/main.rxt");
        for (shell, interpreter) in [(ShellType::Csh, "csh"), (ShellType::Tcsh, "tcsh")] {
            let script = WrapperScript::generate(&ctx, "maya", shell);
            assert!(matches!(&script, WrapperScript::Csh(_)));
            assert!(script
                .content()
                .starts_with(&format!("#!/bin/{interpreter} -f")));
            assert!(script
                .content()
                .contains("source '/tmp/suite/contexts/main.rxt'"));
            assert!(script.content().contains("exec 'maya' $argv:q"));
            assert_eq!(script.file_ext(), "csh");
        }
    }

    #[test]
    fn test_csh_quote_protects_shell_expansions() {
        assert!(csh_quote("!$value`").starts_with("'\\!"));
        assert!(csh_quote("!$value`").contains("$value`"));
    }

    #[test]
    fn test_generate_forward_preserves_arguments_and_quotes_values() {
        let args = vec![
            "env".into(),
            "--input".into(),
            "C:\\build dir\\build.rxt".into(),
        ];
        let env_vars = std::collections::HashMap::from([(
            "REZ_BUILD_PROJECT_NAME".into(),
            "test ' package".into(),
        )]);

        let bash = WrapperScript::generate_forward(
            "C:\\Program Files\\Rez\\rez.exe",
            &args,
            &env_vars,
            Some(Path::new("C:\\build dir")),
            ShellType::Bash,
        )
        .unwrap();
        assert!(bash
            .content()
            .contains("export REZ_BUILD_PROJECT_NAME='test '\\'' package'"));
        assert!(bash.content().contains(
            "'C:\\Program Files\\Rez\\rez.exe' 'env' '--input' 'C:\\build dir\\build.rxt' \"$@\""
        ));

        let cmd = WrapperScript::generate_forward(
            "rez.exe",
            &args,
            &env_vars,
            Some(Path::new("C:\\build dir")),
            ShellType::Cmd,
        )
        .unwrap();
        assert!(cmd
            .content()
            .contains("\"rez.exe\" \"env\" \"--input\" \"C:\\build dir\\build.rxt\" %*"));
        assert!(cmd.content().contains("DisableDelayedExpansion"));

        let csh = WrapperScript::generate_forward(
            "rez",
            &args,
            &env_vars,
            Some(Path::new("/tmp/build dir")),
            ShellType::Csh,
        )
        .unwrap();
        assert!(csh
            .content()
            .contains("exec 'rez' 'env' '--input' 'C:\\build dir\\build.rxt' $argv:q"));

        let powershell = WrapperScript::generate_forward(
            "rez",
            &args,
            &env_vars,
            Some(Path::new("C:\\build dir")),
            ShellType::PowerShell,
        )
        .unwrap();
        assert!(powershell
            .content()
            .contains(ShellType::PowerShell.exit_command()));
        let mut command = vec!["rez".to_owned()];
        command.extend_from_slice(&args);
        assert!(powershell
            .content()
            .contains(&ShellType::PowerShell.join_command(&command, false, Some("$args"))));
        assert!(powershell
            .content()
            .contains("Set-Location -LiteralPath 'C:\\build dir' -ErrorAction Stop"));
    }

    #[test]
    fn test_generate_forward_rejects_invalid_environment_keys() {
        let env_vars = std::collections::HashMap::from([("BAD=KEY".into(), "value".into())]);
        assert!(
            WrapperScript::generate_forward("rez", &[], &env_vars, None, ShellType::Bash).is_err()
        );
    }

    #[test]
    fn test_generate_forward_cmd_preserves_batch_percent_values() {
        let env_vars =
            std::collections::HashMap::from([("REZ_BUILD_PATH".into(), "C:\\build%root".into())]);
        let script =
            WrapperScript::generate_forward("rez.exe", &[], &env_vars, None, ShellType::Cmd)
                .unwrap();
        assert!(script
            .content()
            .contains("set \"REZ_BUILD_PATH=C:\\build%%root\""));
    }

    #[test]
    fn test_cmd_quote_doubles_trailing_backslashes() {
        let argument = "C:\\build\\";
        assert_eq!(cmd_quote(argument), "\"C:\\build\\\\\"");
    }

    // -- Script file_ext --

    #[test]
    fn test_script_ext_variants() {
        let bash = WrapperScript::Bash(String::new());
        let cmd = WrapperScript::Cmd(String::new());
        let ps = WrapperScript::PowerShell(String::new());
        assert_eq!(bash.file_ext(), "sh");
        assert_eq!(cmd.file_ext(), "cmd");
        assert_eq!(ps.file_ext(), "ps1");
        let csh = WrapperScript::Csh(String::new());
        assert_eq!(csh.file_ext(), "csh");
    }

    // -- write_to with temp dir --
    #[test]
    fn test_write_to_creates_file() {
        let dir = std::env::temp_dir().join("rez_wrapper_test");
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("tool.sh");

        let script = WrapperScript::Bash("#!/bin/bash\necho hello\n".into());
        script.write_to(&path).expect("write_to should succeed");

        assert!(path.exists());
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("echo hello"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_write_to_creates_parent_dirs() {
        let dir = std::env::temp_dir()
            .join("rez_wrapper_test2")
            .join("deep")
            .join("dir");
        let _ = fs::remove_dir_all(std::env::temp_dir().join("rez_wrapper_test2"));
        let path = dir.join("tool.cmd");

        let script = WrapperScript::Cmd("@echo off\r\n".into());
        script
            .write_to(&path)
            .expect("write_to should create parent dirs");

        assert!(path.exists());
        let _ = fs::remove_dir_all(std::env::temp_dir().join("rez_wrapper_test2"));
    }

    // -- create_wrapper --

    #[test]
    fn test_create_wrapper_bash() {
        let dir = std::env::temp_dir().join("rez_create_wrapper_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let ctx = PathBuf::from("/tmp/suite/contexts/main.rxt");
        let path = create_wrapper(&dir, &ctx, "maya", ShellType::Bash)
            .expect("create_wrapper should succeed");

        assert_eq!(path.file_name().unwrap().to_str().unwrap(), "maya.sh");
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains(". '/tmp/suite/contexts/main.rxt'"));
        assert!(content.contains("exec 'maya' \"$@\""));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_create_wrapper_powershell() {
        let dir = std::env::temp_dir().join("rez_create_wrapper_ps_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let ctx = PathBuf::from("/opt/contexts/dev.rxt");
        let path = create_wrapper(&dir, &ctx, "houdini", ShellType::PowerShell)
            .expect("create_wrapper should succeed");

        assert_eq!(path.file_name().unwrap().to_str().unwrap(), "houdini.ps1");
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains(". '/opt/contexts/dev.rxt'"));

        let _ = fs::remove_dir_all(&dir);
    }

    // -- create_forwarding_script --

    #[test]
    fn test_create_forwarding_script_bash() {
        let dir = std::env::temp_dir().join("rez_forward_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let path = create_forwarding_script(ForwardingScriptOptions {
            bin_dir: &dir,
            tool_name: "py",
            program: "/usr/bin/python3",
            args: &[],
            env_vars: &std::collections::HashMap::new(),
            working_dir: None,
            shell: ShellType::Bash,
            extensionless_unix: false,
        })
        .expect("create_forwarding_script should succeed");

        assert_eq!(path.file_name().unwrap().to_str().unwrap(), "py.sh");
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("exec '/usr/bin/python3' \"$@\""));

        let _ = fs::remove_dir_all(&dir);
    }

    // -- get_wrapper_template --

    #[test]
    fn test_template_bash() {
        let tmpl = get_wrapper_template(ShellType::Bash);
        assert!(tmpl.contains("#!/bin/bash"));
        assert!(tmpl.contains("{context}"));
        assert!(tmpl.contains("{tool}"));
    }

    #[test]
    fn test_template_cmd() {
        let tmpl = get_wrapper_template(ShellType::Cmd);
        assert!(tmpl.contains("@echo off"));
        assert!(tmpl.contains("{context}"));
        assert!(tmpl.contains("{tool}"));
    }

    #[test]
    fn test_template_powershell() {
        let tmpl = get_wrapper_template(ShellType::PowerShell);
        assert!(tmpl.contains("{context}"));
        assert!(tmpl.contains("{tool}"));
    }

    #[test]
    fn test_template_sh_uses_bash() {
        let tmpl = get_wrapper_template(ShellType::Sh);
        assert!(tmpl.contains("#!/bin/bash"));
    }

    // -- bind_interactive_rez --

    #[test]
    fn test_bind_interactive_rez_bash() {
        let script = bind_interactive_rez(ShellType::Bash);
        assert!(script.contains("_rez_complete_fn"));
        assert!(script.contains("complete -F _rez_complete_fn rez"));
        assert!(script.contains("env"));
        assert!(script.contains("build"));
    }

    #[test]
    fn test_bind_interactive_rez_zsh() {
        let script = bind_interactive_rez(ShellType::Zsh);
        assert!(script.contains("#compdef rez"));
        assert!(script.contains("_describe 'rez commands'"));
        assert!(script.contains("env:Spawn shell"));
    }

    #[test]
    fn test_bind_interactive_rez_pwsh() {
        let script = bind_interactive_rez(ShellType::PowerShell);
        assert!(script.contains("Register-ArgumentCompleter"));
        assert!(script.contains("-CommandName rez"));
        assert!(script.contains("env"));
    }

    #[test]
    fn test_bind_interactive_rez_cmd_empty() {
        let script = bind_interactive_rez(ShellType::Cmd);
        assert!(script.is_empty());
    }

    #[test]
    fn test_get_completion_script_alias() {
        let bash_script = get_completion_script(ShellType::Bash);
        assert_eq!(bash_script, bind_interactive_rez(ShellType::Bash));
    }
}
