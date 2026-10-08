// SPDX-License-Identifier: Apache-2.0

//! Shell types and implementations - bash, cmd, powershell.
//!
//! Ported from Python rez shells.py and shell plugins.
// detect_shell:          Detect current shell from environment

use crate::shell::rex::{Action, ActionInterpreter, OutputStyle, RexValue};
use foundation::errors::{Result, RezError};
use model::platform::Platform;
use std::path::Path;

// =============================================================================
// ShellType enum
// =============================================================================

/// Supported shell types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShellType {
    Bash,
    Sh,
    Csh,
    Tcsh,
    Zsh,
    Cmd,
    PowerShell,
    Gitbash,
}

fn quote_windows_argument(argument: &str) -> String {
    let needs_quotes = argument.is_empty()
        || argument.chars().any(|character| {
            character.is_whitespace()
                || matches!(character, '"' | '&' | '|' | '<' | '>' | '^' | '(' | ')')
        });
    if !needs_quotes {
        return argument.to_owned();
    }

    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for character in argument.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                quoted.push_str(&String::from('\\').repeat(backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.push_str(&String::from('\\').repeat(backslashes));
                quoted.push(character);
                backslashes = 0;
            }
        }
    }
    quoted.push_str(&String::from('\\').repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

impl ShellType {
    /// Parse shell type from name string.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "bash" => Some(Self::Bash),
            "sh" => Some(Self::Sh),
            "csh" => Some(Self::Csh),
            "tcsh" => Some(Self::Tcsh),
            "zsh" => Some(Self::Zsh),
            "cmd" => Some(Self::Cmd),
            "powershell" | "pwsh" => Some(Self::PowerShell),
            "gitbash" | "git-bash" | "git_bash" => Some(Self::Gitbash),
            _ => None,
        }
    }

    /// Canonical shell name.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Sh => "sh",
            Self::Csh => "csh",
            Self::Tcsh => "tcsh",
            Self::Zsh => "zsh",
            Self::Cmd => "cmd",
            Self::PowerShell => "powershell",
            Self::Gitbash => "gitbash",
        }
    }

    /// Executable binary name.
    pub fn executable(&self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Sh => "sh",
            Self::Csh => "csh",
            Self::Tcsh => "tcsh",
            Self::Zsh => "zsh",
            Self::Cmd => "cmd.exe",
            Self::PowerShell => {
                if cfg!(windows) {
                    "powershell.exe"
                } else {
                    "pwsh"
                }
            }
            Self::Gitbash => {
                // Use EXEPATH from Git Bash env to find the right bash.exe,
                // otherwise Windows finds WSL's bash.exe in System32 first
                static GITBASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
                let path = GITBASH.get_or_init(|| {
                    // EXEPATH points to Git's bin/ dir; go up to find usr/bin/bash.exe
                    if let Ok(ep) = std::env::var("EXEPATH") {
                        let git_root = std::path::Path::new(&ep)
                            .parent()
                            .unwrap_or(std::path::Path::new(&ep));
                        let candidate = git_root.join("usr").join("bin").join("bash.exe");
                        if candidate.exists() {
                            return candidate.display().to_string();
                        }
                    }
                    "bash.exe".to_string()
                });
                // Leak to get &'static str (only called once via OnceLock)
                // Safety: this is fine, it's a small string allocated once
                path.as_str()
            }
        }
    }

    /// Script file extension for this shell.
    pub fn file_ext(&self) -> &'static str {
        match self {
            Self::Bash | Self::Sh | Self::Zsh | Self::Gitbash => "sh",
            Self::Csh | Self::Tcsh => "csh",
            Self::Cmd => "bat",
            Self::PowerShell => "ps1",
        }
    }

    /// Check if the shell executable is available on this system.
    pub fn is_available(&self) -> bool {
        find_executable(self.executable(), None).is_some()
    }

    /// Line terminator for script files.
    pub fn line_terminator(&self) -> &'static str {
        match self {
            Self::Cmd => "\r\n",
            _ => "\n",
        }
    }

    /// Environment variable token form, e.g. "${KEY}" or "%KEY%".
    pub fn key_token(&self, key: &str) -> String {
        match self {
            Self::Cmd => format!("%{key}%"),
            Self::PowerShell => format!("${{Env:{key}}}"),
            _ => format!("${{{key}}}"),
        }
    }

    /// Path list separator for this shell.
    /// Unix shells (bash/zsh/csh) always use `:`, even on Windows (Git Bash/MSYS2).
    /// Only native Windows shells (cmd, powershell) use `;`.
    pub fn path_sep(&self) -> &'static str {
        match self {
            Self::Cmd | Self::PowerShell => ";",
            _ => ":",
        }
    }

    /// Join arguments using Rez expansion rules, or quote literal non-Cmd values.
    ///
    /// expand_vars=true preserves Rez's runtime variable expansion. Literal mode
    /// is suitable for generated paths and argv. Cmd retains Windows argument
    /// quoting; percent expansion remains a limitation of its shell API.
    /// forward_args is an optional trusted shell expression for runtime argv
    /// (PowerShell wrappers pass `$args`); evaluate it before native encoding.
    pub fn join_command(
        self,
        args: &[String],
        expand_vars: bool,
        forward_args: Option<&str>,
    ) -> String {
        if args.is_empty() {
            return String::new();
        }
        if self == Self::Cmd {
            return args
                .iter()
                .map(|argument| quote_windows_argument(argument))
                .collect::<Vec<_>>()
                .join(" ");
        }
        let quote_powershell = |argument: &str| {
            if expand_vars {
                format!(
                    "\"{}\"",
                    argument
                        .replace('\u{60}', "\x60\x60")
                        .replace('"', "\x60\"")
                )
            } else {
                format!("'{}'", argument.replace('\'', "''"))
            }
        };
        if self == Self::PowerShell && args[0] != "." {
            let expressions: Vec<_> = args
                .iter()
                .map(|argument| quote_powershell(argument))
                .collect();
            return self.join_powershell(&expressions, forward_args);
        }
        static SAFE_WORD: std::sync::LazyLock<regex::Regex> =
            std::sync::LazyLock::new(|| regex::Regex::new(r"^[\w@%+=:,./-]+$").unwrap());
        let dot_source = self == Self::PowerShell && args[0] == ".";
        let joined = args
            .iter()
            .enumerate()
            .map(|(index, argument)| {
                if dot_source && index == 0 {
                    return ".".to_owned();
                }
                if self == Self::PowerShell {
                    return quote_powershell(argument);
                }
                if !expand_vars {
                    return match self {
                        Self::Csh | Self::Tcsh => BashShell::escape_csh(argument),
                        _ => BashShell::escape(argument),
                    };
                }
                if argument.is_empty() {
                    return "''".to_owned();
                }
                if SAFE_WORD.is_match(argument) {
                    return argument.clone();
                }
                let escaped = if matches!(self, Self::Csh | Self::Tcsh) {
                    // Csh requires single-quoted backtick fragments, unlike POSIX shells.
                    // Escape input quotes before inserting the literal fragments.
                    argument
                        .replace('"', "\"'\"'\"")
                        .replace('\u{60}', "\"'`'\"")
                        .replace('!', "\\!")
                } else {
                    argument
                        .replace('\u{60}', "\\\x60")
                        .replace('"', "\"'\"'\"")
                };
                format!("\"{escaped}\"")
            })
            .collect::<Vec<_>>()
            .join(" ");
        joined
    }

    /// Join trusted PowerShell argument expressions through the shared native binder.
    fn join_powershell(self, expressions: &[String], forward_args: Option<&str>) -> String {
        if expressions.is_empty() {
            return String::new();
        }
        if !cfg!(windows) {
            let forward = forward_args
                .map(|expression| {
                    format!(" @{}", expression.strip_prefix('$').unwrap_or(expression))
                })
                .unwrap_or_default();
            return format!("& {}{forward}", expressions.join(" "));
        }
        let command = &expressions[0];
        let arguments = expressions[1..].join(", ");
        let forward = forward_args
            .map(|expression| format!(" + @({expression})"))
            .unwrap_or_default();
        // Keep the native CommandAST outside the preparation expression: this
        // preserves PowerShell pipelines, stdin, and byte-preserving redirection.
        // Reserved $__rez_rs_* locals hold one evaluated command/argv snapshot.
        // Legacy encoding follows the installed binder's version and mode.
        // Native-only StringBuilder values avoid its in-band --% string marker.
        format!(
            r#"& $(
    $__rez_rs_command = {command}
    $__rez_rs_argv = @({arguments}){forward}
    $__rez_rs_resolved = $ExecutionContext.InvokeCommand.GetCommand($__rez_rs_command,[System.Management.Automation.CommandTypes]::All)
    while ($__rez_rs_resolved -is [System.Management.Automation.AliasInfo]) {{
        $__rez_rs_resolved = $__rez_rs_resolved.ResolvedCommand
    }}
    if ($null -ne $__rez_rs_resolved -and $__rez_rs_resolved.CommandType -eq [System.Management.Automation.CommandTypes]::Application) {{
        $__rez_rs_legacy = $PSVersionTable.PSVersion -lt [version]'7.3'
        if (-not $__rez_rs_legacy) {{
            try {{ $__rez_rs_style = [System.Management.Automation.NativeArgumentPassingStyle]$PSNativeCommandArgumentPassing }} catch {{ $__rez_rs_style = [System.Management.Automation.NativeArgumentPassingStyle]::Legacy }}
            $__rez_rs_legacy = $__rez_rs_style -eq [System.Management.Automation.NativeArgumentPassingStyle]::Legacy -or (
                $__rez_rs_style -eq [System.Management.Automation.NativeArgumentPassingStyle]::Windows -and (
                    [IO.Path]::GetExtension($__rez_rs_resolved.Path) -in '.js','.wsf','.cmd','.bat','.vbs' -or
                    [IO.Path]::GetFileNameWithoutExtension($__rez_rs_resolved.Path) -in 'cmd','cscript','find','sqlcmd','wscript'))
        }}
        $__rez_rs_quote = if ($PSVersionTable.PSVersion.Major -lt 6) {{ '$1$1""' }} else {{ '$1$1\"' }}
        $__rez_rs_argv = @($__rez_rs_argv | ForEach-Object {{
            $__rez_rs_value = [string]$_
            if ($__rez_rs_legacy -and ($__rez_rs_value -eq '' -or $__rez_rs_value -match '[\s"]')) {{
                $__rez_rs_value = '"' + [regex]::Replace([regex]::Replace($__rez_rs_value,'(\\*)"',[string]$__rez_rs_quote),'(\\+)\z','$1$1') + '"'
            }}
            [System.Text.StringBuilder]::new($__rez_rs_value)
        }})
    }}
    # Preserve PowerShell's normal command-not-found and unresolved-alias diagnostics.
    if ($null -eq $__rez_rs_resolved) {{ $__rez_rs_command }} else {{ $__rez_rs_resolved }}
) @__rez_rs_argv"#
        )
    }

    /// Finish a noninteractive script with the most recent command's exit status.
    /// PowerShell preserves Rez's nonzero native status priority, while capturing
    /// the last command's success before any variable lookup changes `$?`.
    pub fn exit_command(self) -> &'static str {
        match self {
            Self::Cmd => "exit /b %errorlevel%",
            Self::Csh | Self::Tcsh => "exit $status",
            Self::PowerShell => {
                "$__rez_rs_success = $?\n$__rez_rs_status = $ExecutionContext.SessionState.PSVariable.GetValue('LASTEXITCODE', 0)\nif ($__rez_rs_status -ne 0) { exit $__rez_rs_status }\nif (-not $__rez_rs_success) { exit 1 }\nexit 0"
            }
            _ => "exit $?",
        }
    }

    /// Build a subprocess that executes shell source without re-quoting it as argv.
    ///
    /// The shell is resolved against the parent PATH here: on Unix, `Command`
    /// searches the child's PATH, which a resolved context may leave without
    /// system directories (they come from the platform package). An unresolved
    /// name is kept, so spawning reports the usual not-found error.
    pub fn command(&self, source: &str) -> std::process::Command {
        let executable =
            find_executable(self.executable(), None).unwrap_or_else(|| self.executable().into());
        let mut command = std::process::Command::new(executable);
        #[cfg(windows)]
        if *self == Self::Cmd {
            use std::os::windows::process::CommandExt;
            // Cmd parses its own source syntax, not CRT-escaped argv. /S removes
            // only this outer pair of quotes, preserving quotes inside the source.
            command
                .args(["/D", "/S", "/C"])
                .raw_arg(format!("\"{source}\""));
            return command;
        }
        command.args(self.command_args(source));
        command
    }

    /// Arguments for shells that accept ordinary argv transport.
    fn command_args(&self, command: &str) -> Vec<String> {
        match self {
            Self::Cmd => vec!["/D".into(), "/C".into(), command.into()],
            Self::PowerShell => vec!["-NoProfile".into(), "-Command".into(), command.into()],
            _ => vec!["-c".into(), command.into()],
        }
    }
}

impl std::fmt::Display for ShellType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

// =============================================================================
// StartupCapabilities
// =============================================================================

/// Describes what startup options a shell actually supports.
#[derive(Debug, Clone, Default)]
pub struct StartupCapabilities {
    pub stdin: bool,
    pub command: bool,
    pub rcfile: bool,
    pub norc: bool,
}

/// Describes how to start up a shell with given options.
#[derive(Debug, Clone)]
pub struct StartupSequence {
    pub stdin: bool,
    pub command: Option<String>,
    pub do_rcfile: bool,
    pub envvar: Option<String>,
    pub files: Vec<String>,
    pub bind_files: Vec<String>,
    pub source_bind_files: bool,
}

// =============================================================================
// Shell trait
// =============================================================================

/// Interface for shell implementations. Each shell also implements ActionInterpreter.
pub trait Shell: ActionInterpreter {
    /// Shell type identifier.
    fn shell_type(&self) -> ShellType;

    /// What startup capabilities this shell supports.
    fn startup_caps(&self) -> StartupCapabilities;

    /// Get accumulated output as a script string.
    fn get_output(&self, style: OutputStyle) -> String;

    /// Get startup sequence for shell with given options.
    /// Returns how to configure shell startup based on requested options.
    fn get_startup_sequence(
        &self,
        _rcfile: Option<&str>,
        _norc: bool,
        _stdin: bool,
        _command: Option<&str>,
    ) -> StartupSequence {
        // Default minimal implementation
        StartupSequence {
            stdin: false,
            command: None,
            do_rcfile: false,
            envvar: None,
            files: Vec::new(),
            bind_files: Vec::new(),
            source_bind_files: false,
        }
    }

    /// Get system PATH by launching shell with norc and echoing PATH.
    /// Returns None if detection fails.
    fn get_syspaths(&self) -> Option<Vec<String>> {
        // Default implementation: detect by spawning shell
        let shell_exe = self.shell_type().executable();
        let norc_arg = match self.shell_type() {
            ShellType::Bash => "--norc",
            ShellType::Sh => "--noprofile",
            ShellType::Zsh => "--no-rcs",
            ShellType::Csh | ShellType::Tcsh => "-f",
            _ => return None, // cmd/powershell don't need syspaths detection
        };

        let code = match self.shell_type() {
            ShellType::Cmd => "echo %PATH%",
            ShellType::PowerShell => "Write-Host $env:PATH",
            _ => "echo $PATH",
        };

        let output = std::process::Command::new(shell_exe)
            .arg(norc_arg)
            .arg("-c")
            .arg(code)
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let path_str = String::from_utf8_lossy(&output.stdout);
        let sep = self.shell_type().path_sep();
        Some(
            path_str
                .trim()
                .split(sep)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect(),
        )
    }

    /// Spawn an interactive or non-interactive subshell.
    fn spawn_shell(&self, context_file: &Path, block: bool) -> Result<()> {
        let exe = self.shell_type().executable();
        let mut cmd = std::process::Command::new(exe);
        cmd.arg(context_file);
        if block {
            cmd.status()
                .map_err(|e| RezError::System(format!("Failed to spawn {exe}: {e}")))?;
        } else {
            cmd.spawn()
                .map_err(|e| RezError::System(format!("Failed to spawn {exe}: {e}")))?;
        }
        Ok(())
    }

    /// Create a context file from a list of rex actions.
    fn create_context_file(&mut self, target: &Path, actions: &[Action]) -> Result<()> {
        for action in actions {
            self.apply_action(action);
        }
        let output = Shell::get_output(self, OutputStyle::File);
        std::fs::write(target, output)
            .map_err(|e| RezError::System(format!("Failed to write context file: {e}")))?;
        Ok(())
    }
}

// =============================================================================
// Helper: apply an Action to any ActionInterpreter
// =============================================================================

/// Helper trait to dispatch an Action to an ActionInterpreter.
pub trait ApplyAction: ActionInterpreter {}
impl<T: ActionInterpreter + ?Sized> ApplyAction for T {}

impl ShellType {
    /// Render Rex segments for the shell and parsing stage; Cmd argv can defer escaping to its CRT quoting stage.
    #[doc(hidden)]
    pub fn render_value(self, value: &RexValue, style: OutputStyle, escape: bool) -> String {
        let value = value.expanduser(None).formatted(|text| {
            if !matches!(self, Self::Cmd | Self::PowerShell) {
                return text.to_owned();
            }
            crate::shell::rex::ENV_VAR_RE
                .replace_all(text, |captures: &regex::Captures<'_>| {
                    let key = captures
                        .get(1)
                        .or_else(|| captures.get(2))
                        .unwrap()
                        .as_str();
                    if self == Self::PowerShell && key.starts_with("Env:") {
                        captures[0].to_owned()
                    } else {
                        self.key_token(key)
                    }
                })
                .into_owned()
        });
        let parts: Vec<String> = value
            .0
            .iter()
            .map(|segment| {
                let s = &segment.text;
                match self {
                    Self::Cmd => {
                        let mut escaped = String::new();
                        for ch in s.chars() {
                            match ch {
                                '%' if segment.literal && style == OutputStyle::File => {
                                    escaped.push_str("%%")
                                }
                                '&' | '<' | '>' | '|' | '^' | '"' | '(' | ')' if escape => {
                                    escaped.push('^');
                                    escaped.push(ch);
                                }
                                _ => escaped.push(ch),
                            }
                        }
                        escaped
                    }
                    Self::PowerShell if segment.literal => format!("'{}'", s.replace('\'', "''")),
                    Self::PowerShell => format!(
                        "\"{}\"",
                        s.replace('\x60', "\x60\x60").replace('"', "\x60\"")
                    ),
                    Self::Csh | Self::Tcsh if segment.literal => BashShell::escape_csh(s),
                    Self::Csh | Self::Tcsh => format!(
                        "\"{}\"",
                        s.replace('"', "\\\"")
                            .replace('\x60', "\"'\x60'\"")
                            .replace('!', "\\!")
                    ),
                    _ if segment.literal => BashShell::escape(s),
                    _ => format!(
                        "\"{}\"",
                        s.replace('\\', "\\\\")
                            .replace('"', "\\\"")
                            .replace('\x60', "\\\x60")
                    ),
                }
            })
            .collect();
        if self == Self::PowerShell && parts.len() > 1 {
            format!("({})", parts.join(" + "))
        } else if parts.is_empty() && self != Self::Cmd {
            "''".into()
        } else {
            parts.concat()
        }
    }

    #[doc(hidden)]
    pub fn render_action(self, action: &Action, style: OutputStyle) -> String {
        let sep = self.path_sep();
        match action {
            Action::Setenv { key, value }
            | Action::Resetenv { key, value, .. }
            | Action::Prependenv { key, value }
            | Action::Appendenv { key, value } => {
                let is_path = model::config::CONFIG.pathed_env_vars.iter().any(|pattern| {
                    regex::Regex::new(&foundation::patterns::glob_to_regex(pattern))
                        .is_ok_and(|pattern| pattern.is_match(key))
                });
                let mut combined = if is_path
                    && matches!(self, Self::Bash | Self::Sh | Self::Zsh | Self::Gitbash)
                {
                    value.formatted(|text| BashShell::new(self).norm_path(text))
                } else {
                    value.clone()
                };
                let reference = RexValue::expandable(match self {
                    Self::Cmd => format!("%{key}%"),
                    Self::PowerShell => format!("${{Env:{key}}}"),
                    _ => format!("${{{key}}}"),
                });
                if matches!(action, Action::Prependenv { .. }) {
                    combined.append(RexValue::literal(sep));
                    combined.append(reference);
                } else if matches!(action, Action::Appendenv { .. }) {
                    let mut head = reference;
                    head.append(RexValue::literal(sep));
                    head.append(combined);
                    combined = head;
                }
                let quoted = self.render_value(&combined, style, true);
                match self {
                    Self::Cmd => format!("set {key}={quoted}"),
                    Self::PowerShell => format!("Set-Item -Path \"Env:{key}\" -Value {quoted}"),
                    Self::Csh | Self::Tcsh => format!("setenv {key} {quoted}"),
                    _ => format!("export {key}={quoted}"),
                }
            }
            Action::Source { path } => {
                let mut path = path.clone();
                if cfg!(windows)
                    && matches!(self, Self::Bash | Self::Sh | Self::Zsh | Self::Gitbash)
                {
                    for segment in &mut path.0 {
                        segment.text = BashShell::new(self).norm_path(&segment.text);
                    }
                }
                let quoted = self.render_value(&path, style, true);
                match self {
                    Self::Cmd if style == OutputStyle::File => {
                        // CALL reparses its command. Defer the pathname substitution
                        // to that second pass, so path contents are never rescanned.
                        use std::hash::BuildHasher;
                        static NEXT: std::sync::atomic::AtomicU64 =
                            std::sync::atomic::AtomicU64::new(0);
                        let key = format!(
                            "REZ_RS_SOURCE_{:016X}_{:X}",
                            std::collections::hash_map::RandomState::new()
                                .hash_one(std::process::id()),
                            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                        );
                        format!("set {key}={quoted}\ncall \"%%{key}%%\"\nset {key}=")
                    }
                    Self::Cmd => format!("call \"{quoted}\""),
                    Self::Csh | Self::Tcsh => format!("source {quoted}"),
                    _ => format!(". {quoted}"),
                }
            }
            Action::Info { msg } | Action::Error { msg } => {
                let quoted = self.render_value(msg, style, true);
                let error = matches!(action, Action::Error { .. });
                match self {
                    Self::Cmd => format!("{}echo({quoted}", if error { "1>&2 " } else { "" }),
                    Self::PowerShell => format!(
                        "{} {quoted}",
                        if error { "Write-Error" } else { "Write-Host" }
                    ),
                    Self::Csh | Self::Tcsh => format!(
                        "printf '%s\\n' {quoted}{}",
                        if error { " >& /dev/stderr" } else { "" }
                    ),
                    _ => format!(
                        "printf '%s\\n' {quoted}{}",
                        if error { " 1>&2" } else { "" }
                    ),
                }
            }
            Action::Command { cmd } => cmd.clone(),
            Action::CommandArgs { args } => {
                let expressions: Vec<_> = args
                    .iter()
                    .map(|arg| self.render_value(arg, style, true))
                    .collect();
                if self == Self::PowerShell {
                    self.join_powershell(&expressions, None)
                } else if self == Self::Cmd {
                    args.iter()
                        .enumerate()
                        .map(|(index, arg)| {
                            let value = self.render_value(arg, style, false);
                            let quoted = quote_windows_argument(&value);
                            if index == 0 {
                                quoted
                            } else {
                                let mut escaped = String::with_capacity(quoted.len());
                                for ch in quoted.chars() {
                                    if matches!(ch, '&' | '|' | '<' | '>' | '^' | '(' | ')' | '"') {
                                        escaped.push('^');
                                    }
                                    escaped.push(ch);
                                }
                                escaped
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                } else {
                    expressions.join(" ")
                }
            }
            Action::Comment { text } => text
                .lines()
                .map(|line| format!("{} {line}", if self == Self::Cmd { "REM" } else { "#" }))
                .collect::<Vec<_>>()
                .join("\n"),
            Action::Shebang { value } => value.clone().unwrap_or_default(),
            Action::Unsetenv { key } => match self {
                Self::Cmd => format!("set {key}="),
                Self::PowerShell => {
                    format!("Remove-Item -ErrorAction SilentlyContinue \"Env:{key}\"")
                }
                Self::Csh | Self::Tcsh => format!("unsetenv {key}"),
                _ => format!("unset {key}"),
            },
            Action::Alias { name, cmd } => match self {
                Self::Cmd => format!("doskey {name}={cmd} $*"),
                Self::PowerShell => format!("function {name}() {{ {cmd} @args }}"),
                Self::Csh | Self::Tcsh => {
                    format!("alias {name} '{}'", cmd.replace('\'', "'\"'\"'"))
                }
                _ => format!("function {name}() {{ {cmd} \"$@\"; }};export -f {name};"),
            },
            Action::Stop { .. } => String::new(),
        }
    }
}

// =============================================================================
// Helper: find executable on PATH
// =============================================================================

/// Find an executable using inherited environment or an explicit build overlay.
/// Both policies share the same PATH/PATHEXT and filesystem checks.
pub fn find_executable(
    name: &str,
    search: Option<(&std::collections::HashMap<String, String>, &Path)>,
) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    let inherited_cwd;
    let cwd = if let Some((_, cwd)) = search {
        cwd
    } else {
        inherited_cwd = std::env::current_dir().ok()?;
        inherited_cwd.as_path()
    };
    let cwd = std::path::absolute(cwd).ok()?;
    let environment = |key: &str| {
        search
            .and_then(|(environment, _)| {
                environment
                    .iter()
                    .find(|(name, _)| {
                        if cfg!(windows) {
                            name.eq_ignore_ascii_case(key)
                        } else {
                            name.as_str() == key
                        }
                    })
                    .map(|(_, value)| std::ffi::OsString::from(value))
            })
            .or_else(|| std::env::var_os(key))
    };
    let program = Path::new(name);
    let mut names = Vec::new();
    if cfg!(windows) && program.extension().is_none() {
        let extensions = environment("PATHEXT").unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
        for extension in extensions
            .to_string_lossy()
            .split(';')
            .filter(|extension| !extension.is_empty())
        {
            names.push(std::path::PathBuf::from(format!("{name}{extension}")));
        }
    }
    names.push(program.to_path_buf());
    let directories = if program.is_absolute() || program.components().count() > 1 {
        vec![cwd.to_path_buf()]
    } else {
        let mut directories = Vec::new();
        if cfg!(windows) {
            directories.push(cwd.to_path_buf());
        }
        if let Some(path) = environment("PATH") {
            directories.extend(std::env::split_paths(&path));
        }
        directories
    };
    for directory in directories {
        let directory = if directory.is_absolute() {
            directory
        } else {
            cwd.join(directory)
        };
        for name in &names {
            let candidate = directory.join(name);
            let Ok(metadata) = std::fs::metadata(&candidate) else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

// =============================================================================
// BashShell
// =============================================================================

/// Bash/sh shell implementation (also used as base for gitbash).
pub struct BashShell {
    lines: Vec<Action>,
    shell_type: ShellType,
}

impl BashShell {
    pub fn new(shell_type: ShellType) -> Self {
        Self {
            lines: Vec::new(),
            shell_type,
        }
    }

    fn addline(&mut self, line: String) {
        self.lines.push(Action::Command { cmd: line });
    }

    /// Normalize paths for bash shells on Windows.
    /// For Gitbash/MSYS2: converts `C:\foo` or `C:/foo` → `/c/foo` (POSIX format)
    /// matching Python rez's `to_posix_path()` in gitbash.py.
    /// MSYS2 bash requires POSIX-style paths in PATH for executable lookup.
    /// For regular bash on non-Windows: returns as-is.
    fn norm_path(&self, s: &str) -> String {
        if !cfg!(windows) {
            return s.to_string();
        }
        // Replace backslashes with forward slashes
        let s = s.replace('\\', "/");
        // For MSYS2/Gitbash: convert drive letter C:/ → /c/
        // This is critical because MSYS2 bash can't find executables
        // in PATH entries using Windows-style paths (C:/...).
        if matches!(
            self.shell_type,
            ShellType::Gitbash | ShellType::Bash | ShellType::Sh | ShellType::Zsh
        ) && s.len() >= 3
            && s.as_bytes()[0].is_ascii_alphabetic()
            && s.as_bytes()[1] == b':'
            && s.as_bytes()[2] == b'/'
        {
            let drive = (s.as_bytes()[0] as char).to_ascii_lowercase();
            return format!("/{}{}", drive, &s[2..]);
        }
        s
    }

    /// Escape a string for bash (simple single-quoting).
    fn escape(s: &str) -> String {
        if s.contains('\'') {
            // Replace ' with '\'' (end quote, escaped quote, start quote)
            format!("'{}'", s.replace('\'', "'\\''"))
        } else {
            format!("'{s}'")
        }
    }

    /// Quote literal Csh/Tcsh values, including apostrophes and history markers.
    fn escape_csh(s: &str) -> String {
        format!("'{}'", s.replace('\'', "'\"'\"'").replace('!', "\\!"))
    }

    /// Quote literal Zsh values; percent escapes belong to prompt formatting.
    fn escape_zsh(s: &str) -> String {
        Self::escape(s)
    }
}

impl ActionInterpreter for BashShell {
    fn apply_action(&mut self, action: &Action) {
        self.lines.push(action.clone());
    }
    fn setenv(&mut self, key: &str, value: &str) {
        self.apply_action(&Action::Setenv {
            key: key.into(),
            value: value.into(),
        });
    }

    fn unsetenv(&mut self, key: &str) {
        self.addline(format!("unset {key}"));
    }

    fn resetenv(&mut self, key: &str, value: &str, _friends: Option<&[String]>) {
        self.apply_action(&Action::Resetenv {
            key: key.into(),
            value: RexValue::expandable(self.norm_path(value)),
            friends: _friends.map(|v| v.to_vec()),
        });
    }

    fn prependenv(&mut self, key: &str, value: &str) {
        self.apply_action(&Action::Prependenv {
            key: key.into(),
            value: RexValue::expandable(self.norm_path(value)),
        });
    }

    fn appendenv(&mut self, key: &str, value: &str) {
        self.apply_action(&Action::Appendenv {
            key: key.into(),
            value: RexValue::expandable(self.norm_path(value)),
        });
    }

    fn alias(&mut self, name: &str, cmd: &str) {
        // Bash uses exported functions for reliable alias behavior
        self.addline(format!(
            "function {name}() {{ {cmd} \"$@\"; }};export -f {name};"
        ));
    }

    fn source(&mut self, path: &str) {
        self.apply_action(&Action::Source { path: path.into() });
    }

    fn command(&mut self, cmd: &str) {
        self.addline(cmd.to_string());
    }

    fn comment(&mut self, text: &str) {
        for line in text.split('\n') {
            self.addline(format!("# {line}"));
        }
    }

    fn shebang(&mut self, value: Option<&str>) {
        let val = value.unwrap_or("#!/bin/bash");
        self.lines.insert(
            0,
            Action::Command {
                cmd: val.to_string(),
            },
        );
    }

    fn info(&mut self, msg: &str) {
        self.apply_action(&Action::Info {
            msg: RexValue::literal(msg),
        });
    }

    fn error(&mut self, msg: &str) {
        self.apply_action(&Action::Error {
            msg: RexValue::literal(msg),
        });
    }

    fn env_sep(&self, _key: &str) -> &str {
        self.shell_type.path_sep()
    }

    fn escape_string(&self, s: &str) -> String {
        match self.shell_type {
            ShellType::Csh | ShellType::Tcsh => Self::escape_csh(s),
            ShellType::Zsh => Self::escape_zsh(s),
            _ => Self::escape(s), // Bash, Sh, Gitbash
        }
    }

    fn get_output(&self) -> &str {
        // Not used directly - Shell::get_output() is preferred
        ""
    }

    fn key_token(&self, key: &str) -> String {
        format!("${{{key}}}")
    }

    fn normalize_path(&self, path: &str) -> String {
        path.to_string()
    }

    fn saferefenv(&mut self, key: &str) {
        // Ensure var is defined (empty if not set) so expansion is safe
        self.addline(format!("export {key}=\"${{{key}:-}}\""));
    }
}

impl Shell for BashShell {
    fn shell_type(&self) -> ShellType {
        self.shell_type
    }

    fn startup_caps(&self) -> StartupCapabilities {
        StartupCapabilities {
            stdin: true,
            command: true,
            rcfile: self.shell_type == ShellType::Bash,
            norc: true,
        }
    }

    fn get_output(&self, style: OutputStyle) -> String {
        let lines: Vec<String> = self
            .lines
            .iter()
            .map(|action| self.shell_type.render_action(action, style))
            .collect();
        match style {
            OutputStyle::File => format!("{}\n", lines.join("\n")),
            OutputStyle::Eval => lines
                .iter()
                .filter(|line| !line.starts_with('#') && !line.starts_with("REM "))
                .map(|line| line.trim_end().trim_end_matches(';'))
                .collect::<Vec<_>>()
                .join(";"),
        }
    }
}

// =============================================================================
// CshShell
// =============================================================================

/// C Shell (csh/tcsh) implementation.
pub struct CshShell {
    lines: Vec<Action>,
    shell_type: ShellType,
}

impl CshShell {
    pub fn new(shell_type: ShellType) -> Self {
        Self {
            lines: Vec::new(),
            shell_type,
        }
    }

    fn addline(&mut self, line: String) {
        self.lines.push(Action::Command { cmd: line });
    }

    /// Reuse the canonical literal Csh/Tcsh quoting policy.
    fn escape(s: &str) -> String {
        BashShell::escape_csh(s)
    }
}

impl ActionInterpreter for CshShell {
    fn apply_action(&mut self, action: &Action) {
        self.lines.push(action.clone());
    }
    fn setenv(&mut self, key: &str, value: &str) {
        self.apply_action(&Action::Setenv {
            key: key.into(),
            value: value.into(),
        });
    }

    fn unsetenv(&mut self, key: &str) {
        self.addline(format!("unsetenv {key}"));
    }

    fn resetenv(&mut self, key: &str, value: &str, _friends: Option<&[String]>) {
        self.apply_action(&Action::Resetenv {
            key: key.into(),
            value: value.into(),
            friends: _friends.map(|v| v.to_vec()),
        });
    }

    fn prependenv(&mut self, key: &str, value: &str) {
        self.apply_action(&Action::Prependenv {
            key: key.into(),
            value: value.into(),
        });
    }

    fn appendenv(&mut self, key: &str, value: &str) {
        self.apply_action(&Action::Appendenv {
            key: key.into(),
            value: value.into(),
        });
    }

    fn alias(&mut self, name: &str, cmd: &str) {
        // Csh syntax: alias name 'command'
        self.addline(format!("alias {name} '{cmd}'"));
    }

    fn source(&mut self, path: &str) {
        self.apply_action(&Action::Source { path: path.into() });
    }

    fn command(&mut self, cmd: &str) {
        self.addline(cmd.to_string());
    }

    fn comment(&mut self, text: &str) {
        for line in text.split('\n') {
            self.addline(format!("# {line}"));
        }
    }

    fn shebang(&mut self, value: Option<&str>) {
        let val = if let Some(v) = value {
            v.to_string()
        } else {
            // Default shebang based on shell type
            match self.shell_type {
                ShellType::Tcsh => "#!/bin/tcsh".to_string(),
                _ => "#!/bin/csh".to_string(),
            }
        };
        self.lines.insert(0, Action::Command { cmd: val });
    }

    fn info(&mut self, msg: &str) {
        self.apply_action(&Action::Info {
            msg: RexValue::literal(msg),
        });
    }

    fn error(&mut self, msg: &str) {
        self.apply_action(&Action::Error {
            msg: RexValue::literal(msg),
        });
    }

    fn env_sep(&self, _key: &str) -> &str {
        self.shell_type.path_sep()
    }

    fn escape_string(&self, s: &str) -> String {
        Self::escape(s)
    }

    fn get_output(&self) -> &str {
        // Not used directly - Shell::get_output() is preferred
        ""
    }

    fn key_token(&self, key: &str) -> String {
        format!("${{{key}}}")
    }

    fn normalize_path(&self, path: &str) -> String {
        path.to_string()
    }

    fn saferefenv(&mut self, key: &str) {
        // Ensure var is defined (empty if not set)
        // Csh has different syntax: if (! $?VAR) setenv VAR ""
        self.addline(format!("if (! $?{key}) setenv {key} \"\""));
    }
}

impl Shell for CshShell {
    fn shell_type(&self) -> ShellType {
        self.shell_type
    }

    fn startup_caps(&self) -> StartupCapabilities {
        StartupCapabilities {
            stdin: true,
            command: true,
            rcfile: false,
            norc: false,
        }
    }

    fn get_output(&self, style: OutputStyle) -> String {
        let lines: Vec<String> = self
            .lines
            .iter()
            .map(|action| self.shell_type.render_action(action, style))
            .collect();
        match style {
            OutputStyle::File => format!("{}\n", lines.join("\n")),
            OutputStyle::Eval => lines
                .iter()
                .filter(|line| !line.starts_with('#'))
                .map(|line| line.trim_end().trim_end_matches(';'))
                .collect::<Vec<_>>()
                .join(";"),
        }
    }
}

// =============================================================================
// CmdShell
// =============================================================================

/// Windows cmd.exe shell implementation.
pub struct CmdShell {
    lines: Vec<Action>,
}

impl CmdShell {
    pub fn new() -> Self {
        Self { lines: Vec::new() }
    }

    fn addline(&mut self, line: String) {
        self.lines.push(Action::Command { cmd: line });
    }

    /// Escape special cmd characters: & < > ^ |
    fn escape(s: &str) -> String {
        let mut result = String::with_capacity(s.len());
        for ch in s.chars() {
            match ch {
                '&' | '<' | '>' | '|' => {
                    result.push('^');
                    result.push(ch);
                }
                '^' => {
                    result.push('^');
                    result.push('^');
                }
                _ => result.push(ch),
            }
        }
        result
    }
}

impl Default for CmdShell {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionInterpreter for CmdShell {
    fn apply_action(&mut self, action: &Action) {
        self.lines.push(action.clone());
    }
    fn setenv(&mut self, key: &str, value: &str) {
        self.apply_action(&Action::Setenv {
            key: key.into(),
            value: value.into(),
        });
    }

    fn unsetenv(&mut self, key: &str) {
        self.addline(format!("set {key}="));
    }

    fn resetenv(&mut self, key: &str, value: &str, _friends: Option<&[String]>) {
        self.apply_action(&Action::Resetenv {
            key: key.into(),
            value: value.into(),
            friends: _friends.map(|v| v.to_vec()),
        });
    }

    fn prependenv(&mut self, key: &str, value: &str) {
        self.apply_action(&Action::Prependenv {
            key: key.into(),
            value: value.into(),
        });
    }

    fn appendenv(&mut self, key: &str, value: &str) {
        self.apply_action(&Action::Appendenv {
            key: key.into(),
            value: value.into(),
        });
    }

    fn alias(&mut self, name: &str, cmd: &str) {
        self.addline(format!("doskey {name}={cmd} $*"));
    }

    fn source(&mut self, path: &str) {
        self.apply_action(&Action::Source { path: path.into() });
    }

    fn command(&mut self, cmd: &str) {
        self.addline(cmd.to_string());
    }

    fn comment(&mut self, text: &str) {
        for line in text.split('\n') {
            self.addline(format!("REM {line}"));
        }
    }

    fn shebang(&mut self, _value: Option<&str>) {
        // cmd.exe doesn't use shebangs
    }

    fn info(&mut self, msg: &str) {
        self.apply_action(&Action::Info {
            msg: RexValue::literal(msg),
        });
    }

    fn error(&mut self, msg: &str) {
        self.apply_action(&Action::Error {
            msg: RexValue::literal(msg),
        });
    }

    fn env_sep(&self, _key: &str) -> &str {
        ";"
    }

    fn escape_string(&self, s: &str) -> String {
        Self::escape(s)
    }

    fn get_output(&self) -> &str {
        ""
    }

    fn key_token(&self, key: &str) -> String {
        format!("%{key}%")
    }

    fn normalize_path(&self, path: &str) -> String {
        path.replace('/', "\\")
    }

    fn expand_env_vars(&self) -> bool {
        true
    }

    fn saferefenv(&mut self, _key: &str) {
        // cmd doesn't need safe ref
    }
}

impl Shell for CmdShell {
    fn shell_type(&self) -> ShellType {
        ShellType::Cmd
    }

    fn startup_caps(&self) -> StartupCapabilities {
        StartupCapabilities {
            stdin: false,
            command: true,
            rcfile: false,
            norc: false,
        }
    }

    fn get_output(&self, style: OutputStyle) -> String {
        let lines: Vec<String> = self
            .lines
            .iter()
            .map(|action| ShellType::Cmd.render_action(action, style))
            .collect();
        match style {
            OutputStyle::File => format!("@echo off\n{}\n", lines.join("\n")),
            OutputStyle::Eval => lines
                .iter()
                .filter(|line| !line.starts_with('#') && !line.starts_with("REM "))
                .map(|line| line.trim_end().trim_end_matches(';'))
                .collect::<Vec<_>>()
                .join(" && "),
        }
    }
}

// =============================================================================
// PowerShellShell
// =============================================================================

/// PowerShell / pwsh shell implementation.
pub struct PowerShellShell {
    lines: Vec<Action>,
}

impl PowerShellShell {
    pub fn new() -> Self {
        Self { lines: Vec::new() }
    }

    fn addline(&mut self, line: String) {
        self.lines.push(Action::Command { cmd: line });
    }

    /// Escape quotes and dollar signs for PowerShell double-quoted strings.
    fn escape_quotes(s: &str) -> String {
        s.replace('"', "`\"").replace('\'', "`'")
    }

    /// Escape dollar signs to prevent variable expansion in literals.
    fn escape_vars(s: &str) -> String {
        s.replace('$', "`$")
    }

    /// Full escape for literal strings in PowerShell.
    fn escape_literal(s: &str) -> String {
        // Escape original backticks before inserting escapes for dollars/quotes.
        Self::escape_quotes(&Self::escape_vars(&s.replace('\u{60}', "\x60\x60")))
    }
}

impl Default for PowerShellShell {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionInterpreter for PowerShellShell {
    fn apply_action(&mut self, action: &Action) {
        self.lines.push(action.clone());
    }
    fn setenv(&mut self, key: &str, value: &str) {
        self.apply_action(&Action::Setenv {
            key: key.into(),
            value: value.into(),
        });
    }

    fn unsetenv(&mut self, key: &str) {
        self.addline(format!(
            "Remove-Item -ErrorAction SilentlyContinue \"Env:{key}\""
        ));
    }

    fn resetenv(&mut self, key: &str, value: &str, _friends: Option<&[String]>) {
        self.apply_action(&Action::Resetenv {
            key: key.into(),
            value: value.into(),
            friends: _friends.map(|v| v.to_vec()),
        });
    }

    fn prependenv(&mut self, key: &str, value: &str) {
        self.apply_action(&Action::Prependenv {
            key: key.into(),
            value: value.into(),
        });
    }

    fn appendenv(&mut self, key: &str, value: &str) {
        self.apply_action(&Action::Appendenv {
            key: key.into(),
            value: value.into(),
        });
    }

    fn alias(&mut self, name: &str, cmd: &str) {
        // PowerShell uses function wrappers for reliable alias + args
        self.addline(format!("function {name}() {{ {cmd} @args }}"));
    }

    fn source(&mut self, path: &str) {
        self.apply_action(&Action::Source { path: path.into() });
    }

    fn command(&mut self, cmd: &str) {
        self.addline(cmd.to_string());
    }

    fn comment(&mut self, text: &str) {
        for line in text.split('\n') {
            self.addline(format!("# {line}"));
        }
    }

    fn shebang(&mut self, _value: Option<&str>) {
        // PowerShell doesn't use shebangs
    }

    fn info(&mut self, msg: &str) {
        self.apply_action(&Action::Info {
            msg: RexValue::literal(msg),
        });
    }

    fn error(&mut self, msg: &str) {
        self.apply_action(&Action::Error {
            msg: RexValue::literal(msg),
        });
    }

    fn env_sep(&self, _key: &str) -> &str {
        ";"
    }

    fn escape_string(&self, s: &str) -> String {
        Self::escape_literal(s)
    }

    fn get_output(&self) -> &str {
        ""
    }

    fn key_token(&self, key: &str) -> String {
        format!("${{Env:{key}}}")
    }

    fn normalize_path(&self, path: &str) -> String {
        if cfg!(windows) {
            path.replace('/', "\\")
        } else {
            path.to_string()
        }
    }

    fn expand_env_vars(&self) -> bool {
        true
    }

    fn live_env(&self) -> bool {
        true
    }

    fn saferefenv(&mut self, _key: &str) {
        // PowerShell doesn't need safe ref - Get-ChildItem handles missing vars
    }
}

impl Shell for PowerShellShell {
    fn shell_type(&self) -> ShellType {
        ShellType::PowerShell
    }

    fn startup_caps(&self) -> StartupCapabilities {
        StartupCapabilities {
            stdin: false,
            command: true,
            rcfile: false,
            norc: false,
        }
    }

    fn get_output(&self, style: OutputStyle) -> String {
        let lines: Vec<String> = self
            .lines
            .iter()
            .map(|action| ShellType::PowerShell.render_action(action, style))
            .collect();
        match style {
            OutputStyle::File => format!("{}\n", lines.join("\n")),
            OutputStyle::Eval => lines
                .iter()
                .filter(|line| !line.starts_with('#') && !line.starts_with("REM "))
                .map(|line| line.trim_end().trim_end_matches(';'))
                .collect::<Vec<_>>()
                .join(" ; "),
        }
    }
}

// =============================================================================
// Factory functions
// =============================================================================

/// Create a shell instance for the given shell type.
pub fn create_shell(shell_type: ShellType) -> Box<dyn Shell> {
    match shell_type {
        ShellType::Bash => Box::new(BashShell::new(ShellType::Bash)),
        ShellType::Sh => Box::new(BashShell::new(ShellType::Sh)),
        ShellType::Gitbash => Box::new(BashShell::new(ShellType::Gitbash)),
        ShellType::Zsh => Box::new(BashShell::new(ShellType::Zsh)),
        ShellType::Csh | ShellType::Tcsh => Box::new(CshShell::new(shell_type)),
        ShellType::Cmd => Box::new(CmdShell::new()),
        ShellType::PowerShell => Box::new(PowerShellShell::new()),
    }
}

/// Detect the current shell from environment variables.
///
/// Check order: REZ_SHELL -> platform default (SHELL on unix, COMSPEC on win).
pub fn detect_shell() -> ShellType {
    // Check REZ_SHELL override first
    if let Ok(shell) = std::env::var("REZ_SHELL") {
        if let Some(st) = ShellType::from_name(&shell) {
            return st;
        }
    }

    match Platform::current() {
        Platform::Windows => {
            // Check if running inside git-bash or MSYS
            if std::env::var("MSYSTEM").is_ok() {
                return ShellType::Gitbash;
            }
            // Check COMSPEC for powershell
            if let Ok(comspec) = std::env::var("COMSPEC") {
                let lower = comspec.to_lowercase();
                if lower.contains("powershell") || lower.contains("pwsh") {
                    return ShellType::PowerShell;
                }
            }
            ShellType::Cmd
        }
        Platform::Linux | Platform::MacOS => {
            if let Ok(shell) = std::env::var("SHELL") {
                let basename = shell.rsplit('/').next().unwrap_or("bash");
                ShellType::from_name(basename).unwrap_or(ShellType::Bash)
            } else {
                ShellType::Bash
            }
        }
    }
}

/// Returns all known shell types.
pub fn get_shell_types() -> Vec<ShellType> {
    vec![
        ShellType::Bash,
        ShellType::Sh,
        ShellType::Csh,
        ShellType::Tcsh,
        ShellType::Zsh,
        ShellType::Cmd,
        ShellType::PowerShell,
        ShellType::Gitbash,
    ]
}

/// Returns shell types available on this system.
pub fn get_available_shells() -> Vec<ShellType> {
    get_shell_types()
        .into_iter()
        .filter(|st| st.is_available())
        .collect()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rex_command_args_use_shared_powershell_binder() {
        let args = vec![
            RexValue::literal("tool"),
            RexValue::literal(""),
            RexValue::literal("a b"),
        ];
        let action = Action::CommandArgs { args };
        assert_eq!(
            ShellType::PowerShell.render_action(&action, OutputStyle::File),
            ShellType::PowerShell.join_command(
                &["tool".into(), "".into(), "a b".into()],
                false,
                None
            )
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_rex_command_args_native_windows() {
        let python = std::process::Command::new("python")
            .args(["-c", "import sys;print(sys.executable)"])
            .output()
            .unwrap();
        assert!(python.status.success());
        let python = String::from_utf8(python.stdout).unwrap().trim().to_owned();
        let values = [
            "",
            "a b",
            "a\"&b ^ (c)|d",
            "x\\",
            "a\"^&|<>b",
            "%REZ_TEST_EXPAND%",
        ];
        for shell in [ShellType::Cmd, ShellType::PowerShell] {
            let mut args = vec![
                RexValue::literal(&python),
                RexValue::literal("-c"),
                RexValue::literal("import sys,json;print(json.dumps(sys.argv[1:]))"),
            ];
            args.extend(values.iter().map(|value| RexValue::literal(*value)));
            let mut mixed = RexValue::literal("literal $REZ_TEST_EXPAND ");
            mixed.append(RexValue::expandable("${REZ_TEST_EXPAND}"));
            args.push(mixed);
            let source = shell.render_action(&Action::CommandArgs { args }, OutputStyle::File);
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(format!("argv.{}", shell.file_ext()));
            std::fs::write(
                &path,
                format!(
                    "{}\n{source}\n",
                    if shell == ShellType::Cmd {
                        "@echo off"
                    } else {
                        ""
                    }
                ),
            )
            .unwrap();
            let output = if shell == ShellType::Cmd {
                std::process::Command::new("cmd.exe")
                    .args(["/d", "/v:off", "/c"])
                    .arg(&path)
                    .env("REZ_TEST_EXPAND", "expanded a b")
                    .output()
                    .unwrap()
            } else {
                std::process::Command::new(shell.executable())
                    .args(["-NoProfile", "-File"])
                    .arg(&path)
                    .env("REZ_TEST_EXPAND", "expanded a b")
                    .output()
                    .unwrap()
            };
            assert!(
                output.status.success(),
                "{shell}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let actual: Vec<String> =
                serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
                    panic!(
                        "{shell}: {error}: {}",
                        String::from_utf8_lossy(&output.stdout)
                    )
                });
            let mut expected: Vec<String> = values.iter().map(|value| value.to_string()).collect();
            expected.push("literal $REZ_TEST_EXPAND expanded a b".into());
            assert_eq!(actual, expected, "{shell}");
        }
    }

    #[test]
    fn script_exit_commands_preserve_shell_status_syntax() {
        assert_eq!(ShellType::Cmd.exit_command(), "exit /b %errorlevel%");
        assert_eq!(ShellType::Csh.exit_command(), "exit $status");
        assert_eq!(ShellType::Tcsh.exit_command(), "exit $status");
        assert_eq!(ShellType::Bash.exit_command(), "exit $?");
        let powershell = ShellType::PowerShell.exit_command();
        assert!(powershell.starts_with("$__rez_rs_success = $?\n"));
        assert!(powershell.contains("PSVariable.GetValue('LASTEXITCODE', 0)"));
        assert!(powershell.contains("if ($__rez_rs_status -ne 0) { exit $__rez_rs_status }"));
        assert!(!powershell.contains("Test-Path"));
    }

    // -- ShellType tests --

    #[test]
    fn test_shell_type_from_name() {
        assert_eq!(ShellType::from_name("bash"), Some(ShellType::Bash));
        assert_eq!(ShellType::from_name("BASH"), Some(ShellType::Bash));
        assert_eq!(ShellType::from_name("cmd"), Some(ShellType::Cmd));
        assert_eq!(
            ShellType::from_name("powershell"),
            Some(ShellType::PowerShell)
        );
        assert_eq!(ShellType::from_name("pwsh"), Some(ShellType::PowerShell));
        assert_eq!(ShellType::from_name("gitbash"), Some(ShellType::Gitbash));
        assert_eq!(ShellType::from_name("git-bash"), Some(ShellType::Gitbash));
        assert_eq!(ShellType::from_name("sh"), Some(ShellType::Sh));
        assert_eq!(ShellType::from_name("csh"), Some(ShellType::Csh));
        assert_eq!(ShellType::from_name("tcsh"), Some(ShellType::Tcsh));
        assert_eq!(ShellType::from_name("unknown"), None);
    }

    #[test]
    fn test_shell_type_name_roundtrip() {
        for st in get_shell_types() {
            assert_eq!(ShellType::from_name(st.name()), Some(st));
        }
    }

    #[test]
    fn test_shell_type_executable() {
        assert_eq!(ShellType::Bash.executable(), "bash");
        assert_eq!(ShellType::Cmd.executable(), "cmd.exe");
        // Gitbash.executable() resolves from EXEPATH or falls back to "bash.exe"
        let gitbash_exe = ShellType::Gitbash.executable();
        assert!(
            gitbash_exe.ends_with("bash.exe"),
            "Expected bash.exe, got: {}",
            gitbash_exe
        );
    }

    #[test]
    fn test_shell_type_file_ext() {
        assert_eq!(ShellType::Bash.file_ext(), "sh");
        assert_eq!(ShellType::Cmd.file_ext(), "bat");
        assert_eq!(ShellType::PowerShell.file_ext(), "ps1");
        assert_eq!(ShellType::Csh.file_ext(), "csh");
    }

    #[test]
    fn test_shell_type_key_token() {
        assert_eq!(ShellType::Bash.key_token("PATH"), "${PATH}");
        assert_eq!(ShellType::Cmd.key_token("PATH"), "%PATH%");
        assert_eq!(ShellType::PowerShell.key_token("PATH"), "${Env:PATH}");
    }

    #[test]
    fn test_shell_type_display() {
        assert_eq!(format!("{}", ShellType::Bash), "bash");
        assert_eq!(format!("{}", ShellType::Cmd), "cmd");
    }

    #[test]
    fn test_join_command_quotes_backticks_for_every_non_cmd_shell() {
        for shell in get_shell_types()
            .into_iter()
            .filter(|shell| *shell != ShellType::Cmd)
        {
            for value in [
                "prefix\u{60}echo\u{60}/path",
                "\u{60}echo\u{60}",
                "trailing\u{60}",
            ] {
                let joined = shell.join_command(&["echo".to_owned(), value.to_owned()], true, None);
                let expected = match shell {
                    ShellType::PowerShell => {
                        format!("& \"echo\" \"{}\"", value.replace('\u{60}', "\x60\x60"))
                    }
                    ShellType::Csh | ShellType::Tcsh => {
                        format!("echo \"{}\"", value.replace('\u{60}', "\"'`'\""))
                    }
                    _ => format!("echo \"{}\"", value.replace('\u{60}', "\\\x60")),
                };
                if shell == ShellType::PowerShell && cfg!(windows) {
                    assert!(joined.contains(&format!(
                        "$__rez_rs_argv = @(\"{}\")",
                        value.replace('\u{60}', "\x60\x60")
                    )));
                } else {
                    assert_eq!(joined, expected, "{shell} must quote literal backticks");
                }
            }
        }
    }

    #[test]
    fn test_join_command_preserves_rez_variable_expansion() {
        for shell in get_shell_types()
            .into_iter()
            .filter(|shell| *shell != ShellType::Cmd)
        {
            let joined = shell.join_command(
                &["echo".to_owned(), "$REZ_TEST_VALUE".to_owned()],
                true,
                None,
            );
            let prefix = if shell == ShellType::PowerShell {
                "& "
            } else {
                ""
            };
            if shell == ShellType::PowerShell && cfg!(windows) {
                assert!(joined.contains("$__rez_rs_argv = @(\"$REZ_TEST_VALUE\")"));
            } else if shell == ShellType::PowerShell {
                assert_eq!(joined, "& \"echo\" \"$REZ_TEST_VALUE\"");
            } else {
                assert_eq!(joined, format!("{prefix}echo \"$REZ_TEST_VALUE\""));
            }
        }
    }

    #[test]
    fn test_join_command_literal_mode_uses_shared_quoting() {
        let value = "path/$VALUE/\u{60}echo\u{60}/it's!";
        for shell in get_shell_types()
            .into_iter()
            .filter(|shell| *shell != ShellType::Cmd)
        {
            let joined = shell.join_command(&[value.to_owned()], false, None);
            let expected = match shell {
                ShellType::PowerShell => format!("& '{}'", value.replace('\'', "''")),
                ShellType::Csh | ShellType::Tcsh => BashShell::escape_csh(value),
                _ => BashShell::escape(value),
            };
            if shell == ShellType::PowerShell && cfg!(windows) {
                assert!(joined.contains(&format!(
                    "$__rez_rs_command = '{}'",
                    value.replace('\'', "''")
                )));
            } else {
                assert_eq!(joined, expected);
            }
        }
        assert_eq!(BashShell::escape_csh("it's!"), "'it'\"'\"'s\\!'");
        assert_eq!(CshShell::escape("it's!"), BashShell::escape_csh("it's!"));
    }

    #[test]
    fn test_join_command_powershell_dot_source_is_operator() {
        let path = "path's/$VALUE/\u{60}file\u{60}.ps1";
        assert_eq!(
            ShellType::PowerShell.join_command(&[".".to_owned(), path.to_owned()], false, None),
            format!(". '{}'", path.replace('\'', "''"))
        );
        let command =
            ShellType::PowerShell.join_command(&["tool".to_owned(), path.to_owned()], false, None);
        if cfg!(windows) {
            assert!(command.contains(&format!(
                "$__rez_rs_argv = @('{}')",
                path.replace('\'', "''")
            )));
        } else {
            assert_eq!(command, format!("& 'tool' '{}'", path.replace('\'', "''")));
        }
    }

    #[test]
    fn test_join_command_native_shell_preserves_backtick_value() {
        for expand_vars in [true, false] {
            let value = if expand_vars {
                "prefix\u{60}echo\u{60}/path"
            } else {
                "path/$VALUE/\u{60}echo\u{60}/it's!"
            };
            let (shell, args) = if cfg!(windows) {
                (
                    ShellType::PowerShell,
                    vec!["Write-Output".to_owned(), value.to_owned()],
                )
            } else {
                // printf and echo are shell builtins, so no external tool is needed.
                (
                    ShellType::Sh,
                    vec!["printf".to_owned(), "%s".to_owned(), value.to_owned()],
                )
            };
            let output = shell
                .command(&shell.join_command(&args, expand_vars, None))
                .output()
                .expect("native shell must be available");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(String::from_utf8_lossy(&output.stdout).trim_end(), value);
        }
    }

    #[cfg(windows)]
    #[test]
    fn test_join_command_native_powershell_dot_source_special_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rez path's$literal\u{60}file\u{60}.ps1");
        std::fs::write(&path, "$rezDotTest = 'loaded'\n").unwrap();
        let shell = ShellType::PowerShell;
        let source = shell.join_command(
            &[".".to_owned(), path.to_string_lossy().into_owned()],
            false,
            None,
        );
        let output = shell
            .command(&format!("{source}; Write-Output $rezDotTest"))
            .output()
            .expect("native PowerShell must be available");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "loaded");
    }

    #[test]
    fn test_structured_shell_values_keep_mixed_segments() {
        let mut value = RexValue::literal("literal $HOME ");
        value.append("$HOME");
        value.append(RexValue::literal(" it's"));
        let action = Action::Setenv {
            key: "VALUE".into(),
            value,
        };
        let bash = ShellType::Bash.render_action(&action, OutputStyle::File);
        assert!(
            bash.contains("'literal $HOME '\"$HOME\"' it'\\''s'"),
            "{bash}"
        );
        let ps = ShellType::PowerShell.render_action(&action, OutputStyle::File);
        assert!(
            ps.contains("('literal $HOME ' + \"${Env:HOME}\" + ' it''s')"),
            "{ps}"
        );
        let csh = ShellType::Csh.render_action(&action, OutputStyle::File);
        assert!(csh.contains("'literal $HOME '\"$HOME\""), "{csh}");
    }

    #[test]
    fn test_native_bash_structured_assignments_messages_and_source() {
        let exe = if cfg!(windows) {
            let path = ShellType::Gitbash.executable().to_owned();
            // Bare bash.exe may select the Windows WSL launcher before PATH.
            if std::path::Path::new(&path).is_file() {
                path
            } else if let Some(resolved) = find_executable(&path, None) {
                resolved
            } else {
                return;
            }
        } else {
            "/bin/sh".into()
        };
        let value = "a\"; printf SIDE; # $HOME \u{60}printf SIDE\u{60} \\ ' é";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source path's$literal\u{60}file\u{60}.sh");
        std::fs::write(&path, "REZ_SOURCED=yes\n").unwrap();
        let mut sh = BashShell::new(ShellType::Bash);
        sh.apply_action(&Action::Setenv {
            key: "VALUE".into(),
            value: RexValue::literal(value),
        });
        sh.apply_action(&Action::Source {
            path: RexValue::literal(sh.norm_path(&path.to_string_lossy())),
        });
        sh.info("-n");
        sh.error("a  b; printf SIDE");
        sh.command("printf '%s\\n' \"$VALUE\" \"$REZ_SOURCED\"");
        for style in [OutputStyle::File, OutputStyle::Eval] {
            let rendered = Shell::get_output(&sh, style);
            let output = std::process::Command::new(&exe)
                .args(["-c", &rendered])
                .output()
                .expect("available Bash must execute");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout),
                format!("-n\n{value}\nyes\n"),
                "exe={exe:?}, style={style:?}, script={rendered:?}, stderr={:?}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stderr),
                "a  b; printf SIDE\n"
            );
        }
        let mut sh = BashShell::new(ShellType::Bash);
        sh.setenv("VALUE", "a\"b\\c $REZ_TEST_EXPAND");
        sh.command("printf '%s' \"$VALUE\"");
        let output = std::process::Command::new(exe)
            .env("REZ_TEST_EXPAND", "expanded")
            .args(["-c", &Shell::get_output(&sh, OutputStyle::File)])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "a\"b\\c expanded");
    }

    #[cfg(windows)]
    #[test]
    fn test_native_rex_pending_environment_uses_interpreter_policy() {
        use crate::shell::rex::RexExecutor;
        for shell in [ShellType::Cmd, ShellType::PowerShell] {
            let mut executor = RexExecutor::new(
                create_shell(shell),
                Some(std::collections::HashMap::new()),
                false,
            );
            executor.manager_mut().set_env_sep("REZ_TEST_LIVE", "|");
            executor.setenv("REZ_TEST_LIVE", "initial");
            executor.command(if shell == ShellType::PowerShell {
                "$Env:REZ_TEST_LIVE = 'runtime'"
            } else {
                "set REZ_TEST_LIVE=runtime"
            });
            executor.appendenv("REZ_TEST_LIVE", "tail");
            executor.prependenv("REZ_TEST_LIVE", "head");
            assert_eq!(
                executor.getenv("REZ_TEST_LIVE").unwrap(),
                "head|initial|tail"
            );
            executor.command(if shell == ShellType::PowerShell {
                "[Console]::WriteLine($Env:REZ_TEST_LIVE)"
            } else {
                "powershell.exe -NoProfile -Command \"[Console]::WriteLine($Env:REZ_TEST_LIVE)\""
            });
            let script = Shell::get_output(executor.interpreter().as_ref(), OutputStyle::File);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("pending.{}", shell.file_ext()));
            std::fs::write(&path, &script).unwrap();
            let output = if shell == ShellType::PowerShell {
                std::process::Command::new("powershell.exe")
                    .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
                    .arg(&path)
                    .output()
                    .unwrap()
            } else {
                std::process::Command::new("cmd.exe")
                    .args(["/d", "/v:off", "/c"])
                    .arg(&path)
                    .output()
                    .unwrap()
            };
            assert!(
                output.status.success(),
                "shell={shell:?}, script={script:?}, stderr={:?}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout).trim_end(),
                if shell == ShellType::PowerShell {
                    "head|runtime|tail"
                } else {
                    "head|initial|tail"
                },
                "shell={shell:?}, script={script:?}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn test_native_rex_expanded_values_retain_literal_and_runtime_segments() {
        use crate::shell::rex::RexExecutor;
        for shell in [ShellType::Cmd, ShellType::PowerShell] {
            let mut executor = RexExecutor::new(
                create_shell(shell),
                Some(std::collections::HashMap::from([
                    ("REZ_TEST_KNOWN".into(), "known".into()),
                    (
                        "REZ_TEST_OPAQUE_SOURCE".into(),
                        "%REZ_TEST_RUNTIME%|$REZ_TEST_RUNTIME".into(),
                    ),
                    (
                        "HOME".into(),
                        "C:/%REZ_TEST_RUNTIME%/$REZ_TEST_RUNTIME".into(),
                    ),
                ])),
                false,
            );
            let mut value = RexValue::literal("$REZ_TEST_RUNTIME|");
            value.append(RexValue::expandable("$REZ_TEST_KNOWN/$REZ_TEST_RUNTIME"));
            executor.setenv("REZ_TEST_VALUE", value.clone());
            executor.resetenv("REZ_TEST_RESET", value, None);
            assert_eq!(
                executor.getenv("REZ_TEST_VALUE").unwrap(),
                "$REZ_TEST_RUNTIME|known/$REZ_TEST_RUNTIME"
            );
            executor.manager_mut().set_env_sep("REZ_TEST_OPAQUE", "|");
            executor.setenv(
                "REZ_TEST_OPAQUE",
                RexValue::literal("%REZ_TEST_RUNTIME%|$REZ_TEST_RUNTIME"),
            );
            executor.appendenv("REZ_TEST_OPAQUE", "tail");
            executor.setenv("REZ_TEST_SUBSTITUTED", "$REZ_TEST_OPAQUE_SOURCE");
            executor.setenv("REZ_TEST_HOME", "~/payload");
            executor.command(if shell == ShellType::PowerShell {
                "[Console]::WriteLine($Env:REZ_TEST_VALUE); [Console]::WriteLine($Env:REZ_TEST_RESET); [Console]::WriteLine($Env:REZ_TEST_OPAQUE); [Console]::WriteLine($Env:REZ_TEST_SUBSTITUTED); [Console]::WriteLine($Env:REZ_TEST_HOME)"
            } else {
                "powershell.exe -NoProfile -Command \"[Console]::WriteLine($Env:REZ_TEST_VALUE); [Console]::WriteLine($Env:REZ_TEST_RESET); [Console]::WriteLine($Env:REZ_TEST_OPAQUE); [Console]::WriteLine($Env:REZ_TEST_SUBSTITUTED); [Console]::WriteLine($Env:REZ_TEST_HOME)\""
            });
            let script = Shell::get_output(executor.interpreter().as_ref(), OutputStyle::File);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("segments.{}", shell.file_ext()));
            std::fs::write(&path, &script).unwrap();
            let mut command = if shell == ShellType::PowerShell {
                let mut command = std::process::Command::new("powershell.exe");
                command.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]);
                command
            } else {
                let mut command = std::process::Command::new("cmd.exe");
                command.args(["/d", "/v:off", "/c"]);
                command
            };
            let output = command
                .arg(&path)
                .env("REZ_TEST_RUNTIME", "runtime")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "shell={shell:?}, script={script:?}, stderr={:?}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
                "$REZ_TEST_RUNTIME|known/runtime\n$REZ_TEST_RUNTIME|known/runtime\n%REZ_TEST_RUNTIME%|$REZ_TEST_RUNTIME|tail\n%REZ_TEST_RUNTIME%|$REZ_TEST_RUNTIME\nC:/%REZ_TEST_RUNTIME%/$REZ_TEST_RUNTIME/payload\n",
                "shell={shell:?}, script={script:?}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn test_native_cmd_literal_messages_and_environment() {
        let value = "a\"&b ^ (c)|d %REZ_TEST_EXPAND%";
        let mut sh = CmdShell::new();
        sh.apply_action(&Action::Setenv {
            key: "REZ_TEST_VALUE".into(),
            value: RexValue::literal(value),
        });
        sh.info(value);
        sh.error(value);
        // Read through PowerShell rather than reinsert a variable into Cmd source.
        sh.command(
            "powershell.exe -NoProfile -Command \"[Console]::WriteLine($Env:REZ_TEST_VALUE)\"",
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("native.cmd");
        std::fs::write(&path, Shell::get_output(&sh, OutputStyle::File)).unwrap();
        let output = std::process::Command::new("cmd.exe")
            .env("REZ_TEST_EXPAND", "wrong")
            .args(["/d", "/v:off", "/c"])
            .arg(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
            format!("{value}\n{value}\n")
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).replace("\r\n", "\n"),
            format!("{value}\n")
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_native_cmd_source_preserves_second_stage_path_and_environment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("%REZ_TEST_EXPAND%^& source.cmd");
        std::fs::write(&path, "@echo off\r\nset REZ_TEST_SOURCED=yes\r\n").unwrap();
        let mut sh = CmdShell::new();
        sh.apply_action(&Action::Source {
            path: RexValue::literal(path.to_string_lossy().into_owned()),
        });
        sh.command("echo %REZ_TEST_SOURCED%");
        let wrapper = dir.path().join("launch.cmd");
        let rendered = Shell::get_output(&sh, OutputStyle::File);
        std::fs::write(&wrapper, &rendered).unwrap();
        let output = std::process::Command::new("cmd.exe")
            .env("REZ_TEST_EXPAND", "wrong")
            .args(["/d", "/v:off", "/c"])
            .arg(wrapper)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "yes");
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let key = rendered
            .lines()
            .find_map(|line| line.strip_prefix("set REZ_RS_SOURCE_"))
            .unwrap()
            .split('=')
            .next()
            .unwrap();
        assert!(rendered.contains(&format!("set REZ_RS_SOURCE_{key}=\n")));
    }

    // -- BashShell tests --

    #[test]
    fn test_bash_setenv() {
        let mut sh = BashShell::new(ShellType::Bash);
        sh.setenv("FOO", "bar");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "export FOO=\"bar\"\n");
    }

    #[test]
    fn test_bash_unsetenv() {
        let mut sh = BashShell::new(ShellType::Bash);
        sh.unsetenv("FOO");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "unset FOO\n");
    }

    #[test]
    fn test_bash_prependenv() {
        let mut sh = BashShell::new(ShellType::Bash);
        sh.prependenv("PATH", "/usr/local/bin");
        let out = Shell::get_output(&sh, OutputStyle::File);
        let sep = ShellType::Bash.path_sep();
        assert_eq!(
            out,
            format!("export PATH=\"/usr/local/bin\"'{sep}'\"${{PATH}}\"\n")
        );
    }

    #[test]
    fn test_bash_appendenv() {
        let mut sh = BashShell::new(ShellType::Bash);
        sh.appendenv("PATH", "/opt/bin");
        let out = Shell::get_output(&sh, OutputStyle::File);
        let sep = ShellType::Bash.path_sep();
        assert_eq!(
            out,
            format!("export PATH=\"${{PATH}}\"'{sep}'\"/opt/bin\"\n")
        );
    }

    #[test]
    fn test_bash_alias() {
        let mut sh = BashShell::new(ShellType::Bash);
        sh.alias("ll", "ls -la");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert!(out.contains("function ll()"));
        assert!(out.contains("ls -la"));
    }

    #[test]
    fn test_bash_source() {
        let mut sh = BashShell::new(ShellType::Bash);
        sh.source("/etc/profile");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, ". \"/etc/profile\"\n");
    }

    #[test]
    fn test_bash_comment() {
        let mut sh = BashShell::new(ShellType::Bash);
        sh.comment("hello world");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "# hello world\n");
    }

    #[test]
    fn test_bash_shebang() {
        let mut sh = BashShell::new(ShellType::Bash);
        sh.setenv("FOO", "bar");
        sh.shebang(None);
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert!(out.starts_with("#!/bin/bash\n"));
        assert!(out.contains("export FOO=\"bar\""));
    }

    #[test]
    fn test_bash_info_error() {
        let mut sh = BashShell::new(ShellType::Bash);
        sh.info("hello");
        sh.error("fail");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert!(out.contains("printf '%s\\n' 'hello'"));
        assert!(out.contains("printf '%s\\n' 'fail' 1>&2"));
    }

    #[test]
    fn test_bash_eval_output() {
        let mut sh = BashShell::new(ShellType::Bash);
        sh.comment("skip me");
        sh.setenv("A", "1");
        sh.setenv("B", "2");
        let out = Shell::get_output(&sh, OutputStyle::Eval);
        // Comments stripped, joined with ;
        assert!(!out.contains('#'));
        assert!(out.contains("export A=\"1\""));
        assert!(out.contains(';'));
    }

    #[test]
    fn test_bash_multiline() {
        let mut sh = BashShell::new(ShellType::Bash);
        sh.setenv("X", "1");
        sh.unsetenv("Y");
        sh.alias("foo", "bar");
        let out = Shell::get_output(&sh, OutputStyle::File);
        let lines: Vec<&str> = out.trim().split('\n').collect();
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn test_bash_escape() {
        assert_eq!(BashShell::escape("hello"), "'hello'");
        assert_eq!(BashShell::escape("it's"), "'it'\\''s'");
    }

    // -- CshShell tests --

    #[test]
    fn test_csh_setenv() {
        let mut sh = CshShell::new(ShellType::Csh);
        sh.setenv("FOO", "bar");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "setenv FOO \"bar\"\n");
    }

    #[test]
    fn test_csh_setenv_with_special_chars() {
        let mut sh = CshShell::new(ShellType::Csh);
        sh.setenv("PATH", "/usr/bin:/usr/local/bin");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "setenv PATH \"/usr/bin:/usr/local/bin\"\n");
    }

    #[test]
    fn test_csh_setenv_with_spaces() {
        let mut sh = CshShell::new(ShellType::Csh);
        sh.setenv("GREETING", "hello world");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "setenv GREETING \"hello world\"\n");
    }

    #[test]
    fn test_csh_unsetenv() {
        let mut sh = CshShell::new(ShellType::Csh);
        sh.unsetenv("FOO");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "unsetenv FOO\n");
    }

    #[test]
    fn test_csh_prependenv() {
        let mut sh = CshShell::new(ShellType::Csh);
        sh.prependenv("PATH", "/usr/local/bin");
        let out = Shell::get_output(&sh, OutputStyle::File);
        let sep = ShellType::Csh.path_sep();
        assert_eq!(
            out,
            format!("setenv PATH \"/usr/local/bin\"'{sep}'\"${{PATH}}\"\n")
        );
    }

    #[test]
    fn test_csh_appendenv() {
        let mut sh = CshShell::new(ShellType::Csh);
        sh.appendenv("PATH", "/opt/bin");
        let out = Shell::get_output(&sh, OutputStyle::File);
        let sep = ShellType::Csh.path_sep();
        assert_eq!(
            out,
            format!("setenv PATH \"${{PATH}}\"'{sep}'\"/opt/bin\"\n")
        );
    }

    #[test]
    fn test_csh_alias() {
        let mut sh = CshShell::new(ShellType::Csh);
        sh.alias("ll", "ls -la");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "alias ll 'ls -la'\n");
    }

    #[test]
    fn test_csh_source() {
        let mut sh = CshShell::new(ShellType::Csh);
        sh.source("/path/to/script.csh");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "source \"/path/to/script.csh\"\n");
    }

    #[test]
    fn test_csh_comment() {
        let mut sh = CshShell::new(ShellType::Csh);
        sh.comment("test comment");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "# test comment\n");
    }

    #[test]
    fn test_csh_shebang() {
        let mut sh = CshShell::new(ShellType::Csh);
        sh.shebang(None);
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert!(out.starts_with("#!/bin/csh"));
    }

    #[test]
    fn test_tcsh_shebang() {
        let mut sh = CshShell::new(ShellType::Tcsh);
        sh.shebang(None);
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert!(out.starts_with("#!/bin/tcsh"));
    }

    #[test]
    fn test_csh_info() {
        let mut sh = CshShell::new(ShellType::Csh);
        sh.info("information message");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "printf '%s\\n' 'information message'\n");
    }

    #[test]
    fn test_csh_error() {
        let mut sh = CshShell::new(ShellType::Csh);
        sh.error("error message");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "printf '%s\\n' 'error message' >& /dev/stderr\n");
    }

    #[test]
    fn test_csh_saferefenv() {
        let mut sh = CshShell::new(ShellType::Csh);
        sh.saferefenv("MYVAR");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "if (! $?MYVAR) setenv MYVAR \"\"\n");
    }

    #[test]
    fn test_create_shell_csh() {
        let sh = create_shell(ShellType::Csh);
        assert_eq!(sh.shell_type(), ShellType::Csh);
    }

    #[test]
    fn test_create_shell_tcsh() {
        let sh = create_shell(ShellType::Tcsh);
        assert_eq!(sh.shell_type(), ShellType::Tcsh);
    }

    // -- CmdShell tests --

    #[test]
    fn test_cmd_setenv() {
        let mut sh = CmdShell::new();
        sh.setenv("FOO", "bar");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "@echo off\nset FOO=bar\n");
    }

    #[test]
    fn test_cmd_unsetenv() {
        let mut sh = CmdShell::new();
        sh.unsetenv("FOO");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "@echo off\nset FOO=\n");
    }

    #[test]
    fn test_cmd_prependenv() {
        let mut sh = CmdShell::new();
        sh.prependenv("PATH", "C:\\bin");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "@echo off\nset PATH=C:\\bin;%PATH%\n");
    }

    #[test]
    fn test_cmd_appendenv() {
        let mut sh = CmdShell::new();
        sh.appendenv("PATH", "C:\\bin");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "@echo off\nset PATH=%PATH%;C:\\bin\n");
    }

    #[test]
    fn test_cmd_alias() {
        let mut sh = CmdShell::new();
        sh.alias("ll", "dir /w");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert!(out.contains("doskey ll=dir /w $*"));
    }

    #[test]
    fn test_cmd_source() {
        let mut sh = CmdShell::new();
        sh.source("setup.bat");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert!(out.contains("=setup.bat\ncall \"%%REZ_RS_SOURCE_"), "{out}");
        assert!(out.lines().last().unwrap().ends_with('='));
    }

    #[test]
    fn test_cmd_comment() {
        let mut sh = CmdShell::new();
        sh.comment("setup");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "@echo off\nREM setup\n");
    }

    #[test]
    fn test_cmd_info_empty() {
        let mut sh = CmdShell::new();
        sh.info("");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "@echo off\necho(\n");
    }

    #[test]
    fn test_cmd_info_text() {
        let mut sh = CmdShell::new();
        sh.info("hello");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "@echo off\necho(hello\n");
    }

    #[test]
    fn test_cmd_escape_special() {
        assert_eq!(CmdShell::escape("a&b"), "a^&b");
        assert_eq!(CmdShell::escape("a<b>c"), "a^<b^>c");
        assert_eq!(CmdShell::escape("a^b"), "a^^b");
        assert_eq!(CmdShell::escape("a|b"), "a^|b");
    }

    #[test]
    fn test_cmd_eval_output() {
        let mut sh = CmdShell::new();
        sh.comment("skip");
        sh.setenv("A", "1");
        sh.setenv("B", "2");
        let out = Shell::get_output(&sh, OutputStyle::Eval);
        assert!(!out.contains("REM"));
        assert!(out.contains("set A=1"));
        assert!(out.contains("&& "));
    }

    #[test]
    fn test_cmd_key_token() {
        let sh = CmdShell::new();
        assert_eq!(sh.key_token("PATH"), "%PATH%");
    }

    #[test]
    fn test_cmd_normalize_path() {
        let sh = CmdShell::new();
        assert_eq!(sh.normalize_path("a/b/c"), "a\\b\\c");
    }

    // -- PowerShellShell tests --

    #[test]
    fn test_ps_setenv() {
        let mut sh = PowerShellShell::new();
        sh.setenv("FOO", "bar");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "Set-Item -Path \"Env:FOO\" -Value \"bar\"\n");
    }

    #[test]
    fn test_ps_unsetenv() {
        let mut sh = PowerShellShell::new();
        sh.unsetenv("FOO");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert!(out.contains("Remove-Item"));
        assert!(out.contains("Env:FOO"));
    }

    #[test]
    fn test_ps_prependenv() {
        let mut sh = PowerShellShell::new();
        sh.prependenv("PATH", "/usr/bin");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert!(out.contains("Set-Item"));
        assert!(
            out.contains("\"/usr/bin\" + ';' + \"${Env:PATH}\""),
            "{out}"
        );
        assert!(
            !out.contains("Get-ChildItem"),
            "PATH composition uses direct environment expansion: {out}"
        );
    }

    #[test]
    fn test_ps_appendenv() {
        let mut sh = PowerShellShell::new();
        sh.appendenv("PATH", "/opt/bin");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert!(out.contains("Set-Item"));
        assert!(
            out.contains("\"${Env:PATH}\" + ';' + \"/opt/bin\""),
            "{out}"
        );
        assert!(
            !out.contains("Get-ChildItem"),
            "PATH composition uses direct environment expansion: {out}"
        );
    }

    #[test]
    fn test_ps_alias() {
        let mut sh = PowerShellShell::new();
        sh.alias("ll", "Get-ChildItem");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert!(out.contains("function ll()"));
        assert!(out.contains("Get-ChildItem @args"));
    }

    #[test]
    fn test_ps_source() {
        let mut sh = PowerShellShell::new();
        sh.source("setup.ps1");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, ". \"setup.ps1\"\n");
    }

    #[test]
    fn test_ps_comment() {
        let mut sh = PowerShellShell::new();
        sh.comment("note");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "# note\n");
    }

    #[test]
    fn test_ps_info() {
        let mut sh = PowerShellShell::new();
        sh.info("hello");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert!(out.contains("Write-Host"));
    }

    #[test]
    fn test_ps_error() {
        let mut sh = PowerShellShell::new();
        sh.error("oops");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert!(out.contains("Write-Error"));
    }

    #[test]
    fn test_ps_literal_escape_preserves_input_backticks() {
        assert_eq!(
            PowerShellShell::escape_literal("before\u{60}nafter"),
            "before\x60\x60nafter"
        );
        assert_eq!(PowerShellShell::escape_literal("$VALUE"), "\x60$VALUE");
        // The literal policy must not change the interpolating env helper.
        assert_eq!(PowerShellShell::escape_quotes("$VALUE"), "$VALUE");
    }

    #[cfg(windows)]
    #[test]
    fn test_ps_native_literal_messages_preserve_values() {
        let value = "before\u{60}nafter; Write-Output SIDE  $VALUE \"quotes\" it's";
        let shell = ShellType::PowerShell;
        let mut info = PowerShellShell::new();
        info.info(value);
        let rendered = Shell::get_output(&info, OutputStyle::File);
        let output = shell
            .command(&rendered)
            .output()
            .expect("native PowerShell must be available");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim_end(), value);

        let mut error = PowerShellShell::new();
        error.error(value);
        let rendered = Shell::get_output(&error, OutputStyle::File);
        // Convert Write-Error to a catchable record and inspect its exact message,
        // independently of PowerShell's host-specific error formatting.
        let command = format!(
            "$ErrorActionPreference = 'Stop'; try {{ {rendered} }} catch {{ \
             [Console]::Write($_.Exception.Message) }}"
        );
        let output = shell
            .command(&command)
            .output()
            .expect("native PowerShell must be available");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), value);
    }

    #[test]
    fn test_ps_escape_quotes() {
        assert_eq!(
            PowerShellShell::escape_quotes("he said \"hi\""),
            "he said `\"hi`\""
        );
        assert_eq!(PowerShellShell::escape_quotes("it's"), "it`'s");
    }

    #[test]
    fn test_ps_escape_vars() {
        assert_eq!(PowerShellShell::escape_vars("$HOME"), "`$HOME");
    }

    #[test]
    fn test_ps_key_token() {
        let sh = PowerShellShell::new();
        assert_eq!(sh.key_token("PATH"), "${Env:PATH}");
    }

    #[test]
    fn test_ps_eval_output() {
        let mut sh = PowerShellShell::new();
        sh.comment("skip");
        sh.setenv("A", "1");
        let out = Shell::get_output(&sh, OutputStyle::Eval);
        assert!(!out.contains('#'));
        assert!(out.contains("Set-Item"));
    }

    // -- Factory & detection tests --

    #[test]
    fn test_create_shell_types() {
        for st in get_shell_types() {
            let shell = create_shell(st);
            assert_eq!(shell.shell_type(), st);
        }
    }

    #[test]
    fn test_detect_shell_returns_valid() {
        let st = detect_shell();
        assert!(get_shell_types().contains(&st));
    }

    #[test]
    fn test_get_shell_types_nonempty() {
        let types = get_shell_types();
        assert!(types.len() >= 7);
    }

    // -- Shell trait tests --

    #[test]
    fn test_startup_caps_bash() {
        let sh = BashShell::new(ShellType::Bash);
        let caps = sh.startup_caps();
        assert!(caps.stdin);
        assert!(caps.command);
        assert!(caps.rcfile);
        assert!(caps.norc);
    }

    #[test]
    fn test_startup_caps_sh() {
        let sh = BashShell::new(ShellType::Sh);
        let caps = sh.startup_caps();
        assert!(caps.stdin);
        assert!(caps.command);
        assert!(!caps.rcfile); // sh doesn't support rcfile
        assert!(caps.norc);
    }

    #[test]
    fn test_startup_caps_cmd() {
        let sh = CmdShell::new();
        let caps = sh.startup_caps();
        assert!(!caps.stdin);
        assert!(caps.command);
        assert!(!caps.rcfile);
        assert!(!caps.norc);
    }

    #[test]
    fn test_startup_caps_ps() {
        let sh = PowerShellShell::new();
        let caps = sh.startup_caps();
        assert!(!caps.stdin);
        assert!(caps.command);
        assert!(!caps.rcfile);
        assert!(!caps.norc);
    }

    // -- Apply action tests --

    #[test]
    fn test_apply_action_setenv() {
        let mut sh = BashShell::new(ShellType::Bash);
        let action = Action::Setenv {
            key: "X".to_string(),
            value: "42".into(),
        };
        sh.apply_action(&action);
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert_eq!(out, "export X=\"42\"\n");
    }

    #[test]
    fn test_apply_action_sequence() {
        let mut sh = CmdShell::new();
        let actions = vec![
            Action::Comment {
                text: "setup".to_string(),
            },
            Action::Setenv {
                key: "A".to_string(),
                value: "1".into(),
            },
            Action::Setenv {
                key: "B".to_string(),
                value: "2".into(),
            },
            Action::Alias {
                name: "foo".to_string(),
                cmd: "bar".to_string(),
            },
        ];
        for a in &actions {
            sh.apply_action(a);
        }
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert!(out.contains("REM setup"));
        assert!(out.contains("set A=1"));
        assert!(out.contains("set B=2"));
        assert!(out.contains("doskey foo=bar"));
    }

    #[test]
    fn test_create_context_file() {
        let mut sh = BashShell::new(ShellType::Bash);
        let actions = vec![
            Action::Setenv {
                key: "REZ_ENV".to_string(),
                value: "test".into(),
            },
            Action::Prependenv {
                key: "PATH".to_string(),
                value: "/rez/bin".into(),
            },
        ];
        let dir = std::env::temp_dir();
        let target = dir.join("rez_test_ctx.sh");
        sh.create_context_file(&target, &actions)
            .expect("write failed");
        let content = std::fs::read_to_string(&target).expect("read failed");
        assert!(content.contains("export REZ_ENV=\"test\""));
        assert!(content.contains("PATH=\"/rez/bin"));
        // Cleanup
        let _ = std::fs::remove_file(&target);
    }

    // -- Saferefenv tests --

    #[test]
    fn test_bash_saferefenv() {
        let mut sh = BashShell::new(ShellType::Bash);
        sh.saferefenv("REZ_ENV_PROMPT");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert!(out.contains("REZ_ENV_PROMPT"));
        assert!(out.contains(":-}"));
    }

    // -- Path separator tests --

    #[test]
    fn test_cmd_path_sep() {
        assert_eq!(ShellType::Cmd.path_sep(), ";");
    }

    #[test]
    fn test_ps_path_sep() {
        assert_eq!(ShellType::PowerShell.path_sep(), ";");
    }

    // -- Shell-specific escape function tests --

    #[test]
    fn test_csh_escape() {
        assert_eq!(BashShell::escape_csh("hello"), "'hello'");
        assert_eq!(BashShell::escape_csh("hello!"), "'hello\\!'");
    }

    #[test]
    fn test_zsh_escape() {
        assert_eq!(BashShell::escape_zsh("hello"), "'hello'");
        assert_eq!(BashShell::escape_zsh("100%"), "'100%'");
        assert_eq!(
            BashShell::escape_zsh("it's 100%"),
            BashShell::escape("it's 100%")
        );
    }

    #[test]
    fn test_shell_type_escape_dispatch() {
        // Test that shell_type correctly dispatches to right escape method
        let bash_sh = BashShell::new(ShellType::Bash);
        assert_eq!(bash_sh.escape_string("test"), "'test'");

        let csh_sh = BashShell::new(ShellType::Csh);
        assert_eq!(csh_sh.escape_string("hello!"), "'hello\\!'");

        let zsh_sh = BashShell::new(ShellType::Zsh);
        assert_eq!(zsh_sh.escape_string("50%"), "'50%'");
    }

    // -- norm_path: POSIX path conversion tests --

    #[test]
    fn test_norm_path_gitbash_drive_letter() {
        // Gitbash must convert C:/ paths to /c/ for MSYS2 PATH lookup
        let sh = BashShell::new(ShellType::Gitbash);
        if cfg!(windows) {
            assert_eq!(sh.norm_path("C:/Users/test/bin"), "/c/Users/test/bin");
            assert_eq!(sh.norm_path("D:/Programs/app"), "/d/Programs/app");
            assert_eq!(sh.norm_path("C:\\Users\\test"), "/c/Users/test");
        } else {
            assert_eq!(sh.norm_path("/usr/bin"), "/usr/bin");
        }
    }

    #[test]
    fn test_norm_path_bash_drive_letter() {
        // Regular Bash on Windows also needs POSIX conversion (MSYS2)
        let sh = BashShell::new(ShellType::Bash);
        if cfg!(windows) {
            assert_eq!(sh.norm_path("C:/WINDOWS/System32"), "/c/WINDOWS/System32");
            assert_eq!(sh.norm_path("/usr/bin"), "/usr/bin"); // Already POSIX
            assert_eq!(sh.norm_path("relative/path"), "relative/path"); // No drive letter
        }
    }

    #[test]
    fn test_norm_path_no_drive_on_unix() {
        // On non-Windows, paths should pass through unchanged
        let sh = BashShell::new(ShellType::Bash);
        if !cfg!(windows) {
            assert_eq!(sh.norm_path("/usr/local/bin"), "/usr/local/bin");
            assert_eq!(sh.norm_path("/home/user/.local"), "/home/user/.local");
        }
    }

    #[test]
    fn test_norm_path_cmd_no_posix() {
        // Cmd shell should NOT convert to POSIX paths
        let sh = CmdShell::new();
        if cfg!(windows) {
            let v = sh.normalize_path("C:/Users/test");
            assert!(v.contains("C:"), "Cmd should keep Windows paths");
        }
    }

    // -- setenv/appendenv output tests --

    #[test]
    fn test_bash_setenv_posix_path() {
        let mut sh = BashShell::new(ShellType::Gitbash);
        if cfg!(windows) {
            sh.setenv("PATH", "C:/Users/test/bin");
            let out = Shell::get_output(&sh, OutputStyle::File);
            assert!(
                out.contains("/c/Users/test/bin"),
                "setenv should use POSIX paths: {out}"
            );
            assert!(
                !out.contains("C:/"),
                "Should NOT contain Windows drive: {out}"
            );
        }
    }

    #[test]
    fn test_bash_appendenv_posix_path() {
        let mut sh = BashShell::new(ShellType::Gitbash);
        if cfg!(windows) {
            sh.appendenv("PATH", "C:/Programs/python/bin");
            let out = Shell::get_output(&sh, OutputStyle::File);
            assert!(
                out.contains("/c/Programs/python/bin"),
                "appendenv should use POSIX paths: {out}"
            );
        }
    }

    #[test]
    fn test_bash_env_vars_no_posix_convert() {
        let mut sh = BashShell::new(ShellType::Gitbash);
        sh.setenv("MY_ROOT", "C:/projects/myproject");
        let out = Shell::get_output(&sh, OutputStyle::File);
        assert!(
            out.contains("C:/projects/myproject"),
            "Only configured pathed keys normalize: {out}"
        );
    }

    // -- path_sep tests --

    #[test]
    fn test_bash_path_sep_always_colon() {
        // Bash/Gitbash should always use : separator, even on Windows
        assert_eq!(ShellType::Bash.path_sep(), ":");
        assert_eq!(ShellType::Gitbash.path_sep(), ":");
        assert_eq!(ShellType::Zsh.path_sep(), ":");
        assert_eq!(ShellType::Csh.path_sep(), ":");
    }

    #[test]
    fn test_executable_lookup_inherited_and_explicit_paths_share_validation() {
        let executable = std::env::current_exe().unwrap();
        let expected = find_executable(executable.to_str().unwrap(), None).unwrap();
        let environment = std::collections::HashMap::from([("PATH".to_owned(), String::new())]);
        let cwd = tempfile::tempdir().unwrap();
        let actual = find_executable(
            executable.to_str().unwrap(),
            Some((&environment, cwd.path())),
        )
        .unwrap();
        assert_eq!(actual, expected);
        assert!(find_executable("", Some((&environment, cwd.path()))).is_none());
    }

    #[test]
    fn test_executable_lookup_respects_effective_path_platform_case_and_mode() {
        let root = tempfile::tempdir().unwrap();
        let tools = root.path().join("tools");
        let cwd = root.path().join("cwd");
        std::fs::create_dir(&tools).unwrap();
        std::fs::create_dir(&cwd).unwrap();
        let name = "rez_rs_lookup_unique_probe";
        let file = tools.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&file, "test executable placeholder").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let mut environment = std::collections::HashMap::from([
            ("PATH".to_owned(), tools.to_string_lossy().into_owned()),
            ("PATHEXT".to_owned(), ".EXE".into()),
        ]);
        #[cfg(unix)]
        {
            assert!(find_executable(name, Some((&environment, &cwd))).is_none());
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let found = find_executable(name, Some((&environment, &cwd))).unwrap();
        assert_eq!(
            std::fs::canonicalize(found).unwrap(),
            std::fs::canonicalize(&file).unwrap()
        );
        environment.remove("PATH");
        environment.insert("Path".into(), tools.to_string_lossy().into_owned());
        #[cfg(windows)]
        assert!(find_executable(name, Some((&environment, &cwd))).is_some());
        #[cfg(unix)]
        {
            environment.insert("PATH".into(), cwd.to_string_lossy().into_owned());
            assert!(find_executable(name, Some((&environment, &cwd))).is_none());
        }
    }

    #[cfg(windows)]
    #[test]
    fn test_executable_lookup_respects_overlay_pathext_order() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("probe.cmd"), "test").unwrap();
        std::fs::write(root.path().join("probe.exe"), "test").unwrap();
        let environment = std::collections::HashMap::from([
            (
                "PATH".to_owned(),
                root.path().to_string_lossy().into_owned(),
            ),
            ("PATHEXT".to_owned(), ".CMD;.EXE".into()),
        ]);
        let found = find_executable("probe", Some((&environment, root.path()))).unwrap();
        assert!(Path::new(&found)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("cmd")));
    }
}
