// SPDX-License-Identifier: Apache-2.0

//! Rex DSL engine for environment manipulation.
//!
//! Ported from Python rez rex.py and rex_bindings.py.
// RexExecutor:        High-level executor combining manager + bindings
// EphemeralsDict:     Storage for ephemeral variables (_.varname pattern)
// VariantBinding:     Read-only variant attributes for rex code

use foundation::errors::{Result, RezError};
use model::config::CONFIG;
use std::collections::{HashMap, HashSet};
use version::Version;

/// A Rex value retains literal and expandable segments until interpretation.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RexSegment {
    pub literal: bool,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct RexValue(pub Vec<RexSegment>);

impl RexValue {
    pub fn literal(text: impl Into<String>) -> Self {
        Self(vec![RexSegment {
            literal: true,
            text: text.into(),
        }])
    }

    pub fn expandable(text: impl Into<String>) -> Self {
        Self(vec![RexSegment {
            literal: false,
            text: text.into(),
        }])
    }

    pub fn append(&mut self, other: impl Into<Self>) {
        for segment in other.into().0 {
            if let Some(last) = self
                .0
                .last_mut()
                .filter(|last| last.literal == segment.literal)
            {
                last.text.push_str(&segment.text);
            } else {
                self.0.push(segment);
            }
        }
    }

    /// Format/expand only the segments for which Rex permits interpolation.
    pub fn formatted(&self, mut format: impl FnMut(&str) -> String) -> Self {
        Self(
            self.0
                .iter()
                .map(|segment| RexSegment {
                    literal: segment.literal,
                    text: if segment.literal {
                        segment.text.clone()
                    } else {
                        format(&segment.text)
                    },
                })
                .collect(),
        )
    }

    #[doc(hidden)]
    pub fn expanduser(&self, home: Option<&str>) -> Self {
        let home = home
            .map(str::to_owned)
            .or_else(|| dirs::home_dir().map(|path| path.to_string_lossy().into_owned()));
        self.formatted(|text| {
            if (text == "~" || text.starts_with("~/") || text.starts_with("~\\")) && home.is_some()
            {
                format!("{}{}", home.as_deref().unwrap(), &text[1..])
            } else {
                text.to_owned()
            }
        })
    }

    fn expanded(
        &self,
        environ: &HashMap<String, String>,
        parent: &HashMap<String, String>,
    ) -> Self {
        let home = environ
            .get("HOME")
            .or_else(|| parent.get("HOME"))
            .cloned()
            .or_else(|| dirs::home_dir().map(|path| path.to_string_lossy().into_owned()));
        let mut result = Self(Vec::new());
        for segment in &self.0 {
            if segment.literal {
                result.append(Self::literal(&segment.text));
                continue;
            }
            // Track substitution origin separately from evaluation eligibility. A current
            // substitution may still contain a parent reference for the second pass,
            // but its final text is opaque data when subsequently rendered by a shell.
            let mut text = segment.text.clone();
            let mut substituted = vec![false; text.len()];
            for values in [environ, parent] {
                let mut next_text = String::new();
                let mut next_origin = Vec::new();
                let mut end = 0;
                for captures in ENV_VAR_RE.captures_iter(&text) {
                    let matched = captures.get(0).unwrap();
                    next_text.push_str(&text[end..matched.start()]);
                    next_origin.extend_from_slice(&substituted[end..matched.start()]);
                    let key = captures
                        .get(1)
                        .or_else(|| captures.get(2))
                        .unwrap()
                        .as_str();
                    if let Some(value) = values.get(key) {
                        next_text.push_str(value);
                        next_origin.extend(std::iter::repeat_n(true, value.len()));
                    } else {
                        next_text.push_str(matched.as_str());
                        next_origin.extend_from_slice(&substituted[matched.start()..matched.end()]);
                    }
                    end = matched.end();
                }
                next_text.push_str(&text[end..]);
                next_origin.extend_from_slice(&substituted[end..]);
                text = next_text;
                substituted = next_origin;
            }
            let expand_home = home.is_some()
                && (text == "~" || text.starts_with("~/") || text.starts_with("~\\"));
            if expand_home {
                result.append(Self::literal(home.as_deref().unwrap()));
                text.remove(0);
                substituted.remove(0);
            }
            // Origins cover UTF-8 bytes, but transitions are emitted only at character
            // boundaries. Whole-pass matching also handles references assembled across
            // a substitution boundary (current A="$", source "${A}B").
            let mut start = 0;
            let mut literal = substituted.first().copied().unwrap_or(false);
            for (index, _) in text.char_indices() {
                if substituted[index] != literal {
                    result.append(Self(vec![RexSegment {
                        literal,
                        text: text[start..index].to_owned(),
                    }]));
                    start = index;
                    literal = substituted[index];
                }
            }
            result.append(Self(vec![RexSegment {
                literal,
                text: text[start..].to_owned(),
            }]));
        }
        result
    }
}

impl std::fmt::Display for RexValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for segment in &self.0 {
            f.write_str(&segment.text)?;
        }
        Ok(())
    }
}

impl PartialEq<str> for RexValue {
    fn eq(&self, other: &str) -> bool {
        self.0
            .iter()
            .flat_map(|segment| segment.text.bytes())
            .eq(other.bytes())
    }
}

impl PartialEq<&str> for RexValue {
    fn eq(&self, other: &&str) -> bool {
        <Self as PartialEq<str>>::eq(self, other)
    }
}

impl From<String> for RexValue {
    fn from(value: String) -> Self {
        Self::expandable(value)
    }
}
impl From<&str> for RexValue {
    fn from(value: &str) -> Self {
        Self::expandable(value)
    }
}
impl From<&String> for RexValue {
    fn from(value: &String) -> Self {
        Self::expandable(value.clone())
    }
}
impl From<&RexValue> for RexValue {
    fn from(value: &RexValue) -> Self {
        value.clone()
    }
}

// =============================================================================
// Actions
// =============================================================================

/// All possible rex actions that manipulate the environment or produce output.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Setenv {
        key: String,
        value: RexValue,
    },
    Unsetenv {
        key: String,
    },
    Resetenv {
        key: String,
        value: RexValue,
        friends: Option<Vec<String>>,
    },
    Prependenv {
        key: String,
        value: RexValue,
    },
    Appendenv {
        key: String,
        value: RexValue,
    },
    Alias {
        name: String,
        cmd: String,
    },
    Source {
        path: RexValue,
    },
    Command {
        cmd: String,
    },
    CommandArgs {
        args: Vec<RexValue>,
    },
    Info {
        msg: RexValue,
    },
    Error {
        msg: RexValue,
    },
    Comment {
        text: String,
    },
    Shebang {
        value: Option<String>,
    },
    Stop {
        msg: RexValue,
    },
}

impl Action {
    /// Returns the canonical command name for this action.
    pub fn name(&self) -> &str {
        match self {
            Action::Setenv { .. } => "setenv",
            Action::Unsetenv { .. } => "unsetenv",
            Action::Resetenv { .. } => "resetenv",
            Action::Prependenv { .. } => "prependenv",
            Action::Appendenv { .. } => "appendenv",
            Action::Alias { .. } => "alias",
            Action::Source { .. } => "source",
            Action::Command { .. } | Action::CommandArgs { .. } => "command",
            Action::Info { .. } => "info",
            Action::Error { .. } => "error",
            Action::Comment { .. } => "comment",
            Action::Shebang { .. } => "shebang",
            Action::Stop { .. } => "stop",
        }
    }

    /// Returns the env var key if this is an env-related action.
    pub fn env_key(&self) -> Option<&str> {
        match self {
            Action::Setenv { key, .. }
            | Action::Unsetenv { key }
            | Action::Resetenv { key, .. }
            | Action::Prependenv { key, .. }
            | Action::Appendenv { key, .. } => Some(key),
            _ => None,
        }
    }
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Action::Setenv { key, value } => write!(f, "setenv({key}, {value})"),
            Action::Unsetenv { key } => write!(f, "unsetenv({key})"),
            Action::Resetenv {
                key,
                value,
                friends,
            } => {
                write!(f, "resetenv({key}, {value}, {friends:?})")
            }
            Action::Prependenv { key, value } => write!(f, "prependenv({key}, {value})"),
            Action::Appendenv { key, value } => write!(f, "appendenv({key}, {value})"),
            Action::Alias { name, cmd } => write!(f, "alias({name}, {cmd})"),
            Action::Source { path } => write!(f, "source({path})"),
            Action::Command { cmd } => write!(f, "command({cmd})"),
            Action::CommandArgs { args } => write!(f, "command({args:?})"),
            Action::Info { msg } => write!(f, "info({msg})"),
            Action::Error { msg } => write!(f, "error({msg})"),
            Action::Comment { text } => write!(f, "comment({text})"),
            Action::Shebang { value } => write!(f, "shebang({value:?})"),
            Action::Stop { msg } => write!(f, "stop({msg})"),
        }
    }
}

// =============================================================================
// OutputStyle
// =============================================================================

/// Style of code output when using Rex.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputStyle {
    /// Code as it would appear in a script file.
    File,
    /// Code in a form that can be evaluated.
    Eval,
}

// =============================================================================
// EnvironmentDict - tracks env var state during rex execution
// =============================================================================

/// Tracks environment variable state during rex execution.
///
/// Checks overrides first, then falls back to the real process environment.
#[derive(Clone, Debug)]
pub struct EnvironmentDict {
    overrides: HashMap<String, String>,
    /// Parent environment snapshot (captured at creation time).
    parent: HashMap<String, String>,
}

impl EnvironmentDict {
    /// Create from optional parent environment. If None, captures std::env.
    pub fn new(parent: Option<HashMap<String, String>>) -> Self {
        let parent = parent.unwrap_or_else(|| std::env::vars().collect());
        Self {
            overrides: HashMap::new(),
            parent,
        }
    }

    /// Get env var value: check overrides first, then parent.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.overrides
            .get(key)
            .or_else(|| self.parent.get(key))
            .map(|s| s.as_str())
    }

    /// Set an env var override.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.overrides.insert(key.into(), value.into());
    }

    /// Remove an env var override.
    pub fn unset(&mut self, key: &str) {
        self.overrides.remove(key);
    }

    /// Whether a key exists in overrides or parent.
    pub fn contains(&self, key: &str) -> bool {
        self.overrides.contains_key(key) || self.parent.contains_key(key)
    }

    /// Whether a key exists in the overrides (modifications made by rex).
    pub fn has_override(&self, key: &str) -> bool {
        self.overrides.contains_key(key)
    }

    /// Append value to existing env var using separator.
    pub fn append(&mut self, key: &str, value: &str, sep: &str) {
        let current = self.get(key).map(|s| s.to_string()).unwrap_or_default();
        let new_val = if current.is_empty() {
            value.to_string()
        } else {
            format!("{current}{sep}{value}")
        };
        self.overrides.insert(key.to_string(), new_val);
    }

    /// Prepend value to existing env var using separator.
    pub fn prepend(&mut self, key: &str, value: &str, sep: &str) {
        let current = self.get(key).map(|s| s.to_string()).unwrap_or_default();
        let new_val = if current.is_empty() {
            value.to_string()
        } else {
            format!("{value}{sep}{current}")
        };
        self.overrides.insert(key.to_string(), new_val);
    }

    /// Returns all overrides as a snapshot.
    pub fn overrides(&self) -> &HashMap<String, String> {
        &self.overrides
    }

    /// Returns the parent environment.
    pub fn parent_env(&self) -> &HashMap<String, String> {
        &self.parent
    }

    /// Merge overrides into a complete environment (parent + overrides).
    pub fn merged(&self) -> HashMap<String, String> {
        let mut result = self.parent.clone();
        for (k, v) in &self.overrides {
            result.insert(k.clone(), v.clone());
        }
        result
    }
}

// =============================================================================
// ActionInterpreter trait
// =============================================================================

/// Shell-specific output interface for rex actions.
///
/// Implementations generate shell code (bash, cmd, powershell) or apply
/// changes to an in-memory environment (Python-style interpreter).
pub trait ActionInterpreter {
    /// Dispatch the canonical action; shells override this to retain segments.
    fn apply_action(&mut self, action: &Action) {
        match action {
            Action::Setenv { key, value } => self.setenv(key, &value.to_string()),
            Action::Unsetenv { key } => self.unsetenv(key),
            Action::Resetenv {
                key,
                value,
                friends,
            } => self.resetenv(key, &value.to_string(), friends.as_deref()),
            Action::Prependenv { key, value } => self.prependenv(key, &value.to_string()),
            Action::Appendenv { key, value } => self.appendenv(key, &value.to_string()),
            Action::Alias { name, cmd } => self.alias(name, cmd),
            Action::Source { path } => self.source(&path.to_string()),
            Action::Command { cmd } => self.command(cmd),
            Action::CommandArgs { .. } => self.command(
                &crate::shell::ShellType::Bash
                    .render_action(action, crate::shell::OutputStyle::File),
            ),
            Action::Info { msg } => self.info(&msg.to_string()),
            Action::Error { msg } => self.error(&msg.to_string()),
            Action::Comment { text } => self.comment(text),
            Action::Shebang { value } => self.shebang(value.as_deref()),
            Action::Stop { .. } => {}
        }
    }

    fn setenv(&mut self, key: &str, value: &str);
    fn unsetenv(&mut self, key: &str);
    fn resetenv(&mut self, key: &str, value: &str, friends: Option<&[String]>);
    fn prependenv(&mut self, key: &str, value: &str);
    fn appendenv(&mut self, key: &str, value: &str);
    fn alias(&mut self, name: &str, cmd: &str);
    fn source(&mut self, path: &str);
    fn command(&mut self, cmd: &str);
    fn comment(&mut self, text: &str);
    fn shebang(&mut self, value: Option<&str>);
    fn info(&mut self, msg: &str);
    fn error(&mut self, msg: &str);

    /// Returns the path separator for this interpreter (e.g., ":" or ";").
    fn env_sep(&self, _key: &str) -> &str {
        path_sep()
    }

    /// Escape a string for this interpreter's syntax.
    /// Default implementation returns unescaped string.
    fn escape_string(&self, s: &str) -> String {
        s.to_string()
    }

    /// Returns the recorded output (shell code, etc.).
    fn get_output(&self) -> &str;

    /// Whether this interpreter expands env vars itself.
    fn expand_env_vars(&self) -> bool {
        false
    }

    /// Whether pending mutations can reference the executing shell's current environment.
    fn live_env(&self) -> bool {
        !self.expand_env_vars()
    }

    /// Token form of an env var key, e.g. "${KEY}" for sh, "%KEY%" for cmd.
    fn key_token(&self, key: &str) -> String {
        format!("${{{key}}}")
    }

    /// Normalize a path for this interpreter.
    fn normalize_path(&self, path: &str) -> String {
        path.to_string()
    }

    /// Safe reference to an env var (make it safe to expand even if undefined).
    fn saferefenv(&mut self, _key: &str) {}
}

/// Blanket impl for boxed trait objects (enables RexExecutor<Box<dyn Shell>>).
impl<T: ActionInterpreter + ?Sized> ActionInterpreter for Box<T> {
    fn apply_action(&mut self, action: &Action) {
        (**self).apply_action(action)
    }
    fn setenv(&mut self, key: &str, value: &str) {
        (**self).setenv(key, value)
    }
    fn unsetenv(&mut self, key: &str) {
        (**self).unsetenv(key)
    }
    fn resetenv(&mut self, key: &str, value: &str, friends: Option<&[String]>) {
        (**self).resetenv(key, value, friends)
    }
    fn prependenv(&mut self, key: &str, value: &str) {
        (**self).prependenv(key, value)
    }
    fn appendenv(&mut self, key: &str, value: &str) {
        (**self).appendenv(key, value)
    }
    fn alias(&mut self, name: &str, cmd: &str) {
        (**self).alias(name, cmd)
    }
    fn source(&mut self, path: &str) {
        (**self).source(path)
    }
    fn command(&mut self, cmd: &str) {
        (**self).command(cmd)
    }
    fn comment(&mut self, text: &str) {
        (**self).comment(text)
    }
    fn shebang(&mut self, value: Option<&str>) {
        (**self).shebang(value)
    }
    fn info(&mut self, msg: &str) {
        (**self).info(msg)
    }
    fn error(&mut self, msg: &str) {
        (**self).error(msg)
    }
    fn env_sep(&self, key: &str) -> &str {
        (**self).env_sep(key)
    }
    fn escape_string(&self, s: &str) -> String {
        (**self).escape_string(s)
    }
    fn get_output(&self) -> &str {
        (**self).get_output()
    }
    fn expand_env_vars(&self) -> bool {
        (**self).expand_env_vars()
    }
    fn live_env(&self) -> bool {
        (**self).live_env()
    }
    fn key_token(&self, key: &str) -> String {
        (**self).key_token(key)
    }
    fn normalize_path(&self, path: &str) -> String {
        (**self).normalize_path(path)
    }
    fn saferefenv(&mut self, key: &str) {
        (**self).saferefenv(key)
    }
}

/// Canonical Rex environment reference grammar for expansion and shell rendering.
#[doc(hidden)]
pub static ENV_VAR_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"\$\{([^}]+)\}|\$([A-Za-z_][A-Za-z0-9_]*)").unwrap()
});

/// Platform path separator.
fn path_sep() -> &'static str {
    if cfg!(windows) {
        ";"
    } else {
        ":"
    }
}

// =============================================================================
// OutputInterpreter - records actions as generic shell-like strings
// =============================================================================

/// Records rex actions as human-readable shell-like strings.
///
/// Useful for previewing what a rex session would do, or for generating
/// generic (non-shell-specific) output.
pub struct OutputInterpreter {
    output: String,
    pathsep: String,
}

impl OutputInterpreter {
    pub fn new() -> Self {
        Self {
            output: String::new(),
            pathsep: path_sep().to_string(),
        }
    }

    fn emit(&mut self, line: &str) {
        if !self.output.is_empty() {
            self.output.push('\n');
        }
        self.output.push_str(line);
    }
}

impl Default for OutputInterpreter {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionInterpreter for OutputInterpreter {
    fn setenv(&mut self, key: &str, value: &str) {
        self.emit(&format!("export {key}={value}"));
    }

    fn unsetenv(&mut self, key: &str) {
        self.emit(&format!("unset {key}"));
    }

    fn resetenv(&mut self, key: &str, value: &str, _friends: Option<&[String]>) {
        self.emit(&format!("export {key}={value}"));
    }

    fn prependenv(&mut self, key: &str, value: &str) {
        let sep = &self.pathsep;
        self.emit(&format!("export {key}={value}{sep}${{{key}}}"));
    }

    fn appendenv(&mut self, key: &str, value: &str) {
        let sep = &self.pathsep;
        self.emit(&format!("export {key}=${{{key}}}{sep}{value}"));
    }

    fn alias(&mut self, name: &str, cmd: &str) {
        self.emit(&format!("alias {name}='{cmd}'"));
    }

    fn source(&mut self, path: &str) {
        self.emit(&format!("source {path}"));
    }

    fn command(&mut self, cmd: &str) {
        self.emit(cmd);
    }

    fn comment(&mut self, text: &str) {
        self.emit(&format!("# {text}"));
    }

    fn shebang(&mut self, value: Option<&str>) {
        let val = value.unwrap_or("#!/bin/bash");
        // Shebang goes at the start
        if self.output.is_empty() {
            self.output = val.to_string();
        } else {
            self.output = format!("{val}\n{}", self.output);
        }
    }

    fn info(&mut self, msg: &str) {
        self.emit(&format!("echo {msg}"));
    }

    fn error(&mut self, msg: &str) {
        self.emit(&format!("echo {msg} >&2"));
    }

    fn get_output(&self) -> &str {
        &self.output
    }
}

// =============================================================================
// PythonInterpreter - applies changes to a HashMap (in-memory env)
// =============================================================================

/// Applies rex actions to an in-memory environment HashMap.
///
/// Equivalent to the Python `Python` interpreter class in rex.py.
/// Used when you want to capture the resulting environment rather than
/// produce shell code.
pub struct PythonInterpreter {
    env: HashMap<String, String>,
}

impl PythonInterpreter {
    pub fn new() -> Self {
        Self {
            env: HashMap::new(),
        }
    }

    /// Returns the resulting environment.
    pub fn env(&self) -> &HashMap<String, String> {
        &self.env
    }

    /// Consume and return the resulting environment.
    pub fn into_env(self) -> HashMap<String, String> {
        self.env
    }
}

impl Default for PythonInterpreter {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionInterpreter for PythonInterpreter {
    fn setenv(&mut self, key: &str, value: &str) {
        self.env.insert(key.to_string(), value.to_string());
    }

    fn unsetenv(&mut self, key: &str) {
        self.env.remove(key);
    }

    fn resetenv(&mut self, key: &str, value: &str, _friends: Option<&[String]>) {
        self.env.insert(key.to_string(), value.to_string());
    }

    fn prependenv(&mut self, key: &str, value: &str) {
        let sep = self.env_sep(key);
        let current = self.env.get(key).cloned().unwrap_or_default();
        let new_val = if current.is_empty() {
            value.to_string()
        } else {
            format!("{value}{sep}{current}")
        };
        self.env.insert(key.to_string(), new_val);
    }

    fn appendenv(&mut self, key: &str, value: &str) {
        let sep = self.env_sep(key);
        let current = self.env.get(key).cloned().unwrap_or_default();
        let new_val = if current.is_empty() {
            value.to_string()
        } else {
            format!("{current}{sep}{value}")
        };
        self.env.insert(key.to_string(), new_val);
    }

    fn alias(&mut self, _name: &str, _cmd: &str) {
        // No-op in Python interpreter
    }

    fn source(&mut self, _path: &str) {
        // No-op in Python interpreter
    }

    fn command(&mut self, _cmd: &str) {
        // No-op in Python interpreter (would need subprocess in full impl)
    }

    fn comment(&mut self, _text: &str) {
        // No-op
    }

    fn shebang(&mut self, _value: Option<&str>) {
        // No-op
    }

    fn info(&mut self, _msg: &str) {
        // No-op in passive mode (default for in-memory)
    }

    fn error(&mut self, _msg: &str) {
        // No-op in passive mode
    }

    fn expand_env_vars(&self) -> bool {
        true
    }

    fn get_output(&self) -> &str {
        // PythonInterpreter returns env dict, not string output
        ""
    }
}

// =============================================================================
// ActionManager - bookkeeping: tracks env values, dispatches to interpreter
// =============================================================================

/// Handles rex execution bookkeeping: tracks env var values and dispatches
/// actions to the `ActionInterpreter`.
///
/// Equivalent to Python's ActionManager class.
pub struct ActionManager<I: ActionInterpreter> {
    pub interpreter: I,
    /// Accumulated actions.
    pub actions: Vec<Action>,
    /// Current environment state (expanded values).
    environ: HashMap<String, String>,
    /// Parent environment snapshot.
    parent_environ: HashMap<String, String>,
    /// Env var separator overrides per key.
    env_sep_map: HashMap<String, String>,
    /// Vars to merge with parent on first prepend/append (from config parent_variables).
    parent_variables: HashSet<String>,
    /// If true, all vars merge with parent on first prepend/append (from config all_parent_variables).
    all_parent_variables: bool,
}

impl<I: ActionInterpreter> ActionManager<I> {
    /// Create a new manager with the given interpreter and optional parent env.
    ///
    /// Uses CONFIG.parent_variables and CONFIG.all_parent_variables to determine
    /// which vars merge with parent on first prepend/append (Python rez parity).
    pub fn new(interpreter: I, parent_environ: Option<HashMap<String, String>>) -> Self {
        let parent = parent_environ.unwrap_or_else(|| std::env::vars().collect());
        let parent_variables: HashSet<String> = CONFIG.parent_variables.iter().cloned().collect();
        Self {
            interpreter,
            actions: Vec::new(),
            environ: HashMap::new(),
            parent_environ: parent,
            env_sep_map: CONFIG
                .env_var_separators
                .iter()
                .map(|(key, value)| {
                    let key = if cfg!(windows) && key.eq_ignore_ascii_case("path") {
                        "PATH".into()
                    } else {
                        key.clone()
                    };
                    (key, value.clone())
                })
                .collect(),
            parent_variables,
            all_parent_variables: CONFIG.all_parent_variables,
        }
    }

    #[doc(hidden)]
    pub fn parent_env(&self) -> &HashMap<String, String> {
        &self.parent_environ
    }

    #[doc(hidden)]
    pub fn separators(&self) -> &HashMap<String, String> {
        &self.env_sep_map
    }

    /// Set a custom env var separator for a given key name.
    pub fn set_env_sep(&mut self, key: impl Into<String>, sep: impl Into<String>) {
        self.env_sep_map
            .insert(self.path_key(&key.into()), sep.into());
    }

    /// Get the separator for a given env var.
    #[doc(hidden)]
    pub fn env_sep(&self, key: &str) -> String {
        self.env_sep_map
            .get(key)
            .or_else(|| self.env_sep_map.get(""))
            .cloned()
            .unwrap_or_else(|| self.interpreter.env_sep(key).to_string())
    }

    /// Expand current environment references first, then parent references and home.
    pub fn expandvars(&self, value: impl Into<RexValue>) -> String {
        value
            .into()
            .expanded(&self.environ, &self.parent_environ)
            .to_string()
    }

    /// On Windows, PATH and Path are the same; use canonical "PATH" (all caps).
    #[doc(hidden)]
    pub fn path_key(&self, key: &str) -> String {
        if cfg!(windows) && key.eq_ignore_ascii_case("path") {
            "PATH".into()
        } else {
            key.to_string()
        }
    }

    /// For PATH-like keys on Windows, parent may have "Path" or "PATH" or "path".
    fn parent_get_path_value(&self) -> Option<&String> {
        self.parent_environ
            .get("PATH")
            .or_else(|| self.parent_environ.get("Path"))
            .or_else(|| self.parent_environ.get("path"))
    }

    /// Check if path-like key exists in parent (case-insensitive on Windows).
    fn parent_has_path(&self) -> bool {
        self.parent_environ.contains_key("PATH")
            || self.parent_environ.contains_key("Path")
            || self.parent_environ.contains_key("path")
    }

    /// Check if an env var is defined in our env or parent env.
    pub fn defined(&self, key: &str) -> bool {
        let k = self.path_key(&self.expandvars(key));
        if cfg!(windows) && k == "PATH" {
            return self.environ.contains_key(&k) || self.parent_has_path();
        }
        self.environ.contains_key(&k) || self.parent_environ.contains_key(&k)
    }

    /// Check if an env var is undefined.
    pub fn undefined(&self, key: &str) -> bool {
        !self.defined(key)
    }

    /// Get env var value, checking our env then parent.
    pub fn getenv(&self, key: &str) -> Result<&str> {
        let k = self.path_key(&self.expandvars(key));
        if let Some(v) = self.environ.get(&k) {
            Ok(v.as_str())
        } else if cfg!(windows) && k == "PATH" {
            self.parent_get_path_value()
                .map(|s| s.as_str())
                .ok_or_else(|| {
                    RezError::RexUndefinedVariable(format!(
                        "Referenced undefined environment variable: {key}"
                    ))
                })
        } else if let Some(v) = self.parent_environ.get(&k) {
            Ok(v.as_str())
        } else {
            Err(RezError::RexUndefinedVariable(format!(
                "Referenced undefined environment variable: {key}"
            )))
        }
    }

    // -- Action methods --

    pub fn setenv(&mut self, key: &str, value: impl Into<RexValue>) {
        let raw_key = self.path_key(key);
        let k = self.path_key(&self.expandvars(key));
        let value = value.into();
        let expanded = value.expanded(&self.environ, &self.parent_environ);
        let rendered = if self.interpreter.expand_env_vars() {
            expanded.clone()
        } else {
            value.expanduser(
                self.environ
                    .get("HOME")
                    .or_else(|| self.parent_environ.get("HOME"))
                    .map(String::as_str),
            )
        };
        self.actions.push(Action::Setenv {
            key: raw_key.clone(),
            value: value.clone(),
        });
        self.environ.insert(k.clone(), expanded.to_string());
        self.interpreter.apply_action(&Action::Setenv {
            key: if self.interpreter.expand_env_vars() {
                k
            } else {
                raw_key
            },
            value: rendered,
        });
    }

    pub fn unsetenv(&mut self, key: &str) {
        let raw_key = self.path_key(key);
        let k = self.path_key(&self.expandvars(key));
        self.actions.push(Action::Unsetenv {
            key: raw_key.clone(),
        });
        self.environ.remove(&k);
        self.interpreter
            .unsetenv(if self.interpreter.expand_env_vars() {
                &k
            } else {
                &raw_key
            });
    }

    pub fn resetenv(
        &mut self,
        key: &str,
        value: impl Into<RexValue>,
        friends: Option<Vec<String>>,
    ) {
        let raw_key = self.path_key(key);
        let k = self.path_key(&self.expandvars(key));
        let value = value.into();
        let expanded = value.expanded(&self.environ, &self.parent_environ);
        let rendered = if self.interpreter.expand_env_vars() {
            expanded.clone()
        } else {
            value.expanduser(
                self.environ
                    .get("HOME")
                    .or_else(|| self.parent_environ.get("HOME"))
                    .map(String::as_str),
            )
        };
        let action = Action::Resetenv {
            key: raw_key.clone(),
            value: value.clone(),
            friends: friends.clone(),
        };
        self.actions.push(action);
        self.environ.insert(k.clone(), expanded.to_string());
        self.interpreter.apply_action(&Action::Resetenv {
            key: if self.interpreter.expand_env_vars() {
                k
            } else {
                raw_key
            },
            value: rendered,
            friends,
        });
    }

    pub fn prependenv(&mut self, key: &str, value: impl Into<RexValue>) {
        self.modifyenv(key, value.into(), true);
    }

    pub fn appendenv(&mut self, key: &str, value: impl Into<RexValue>) {
        self.modifyenv(key, value.into(), false);
    }

    fn modifyenv(&mut self, key: &str, value: RexValue, prepend: bool) {
        let raw_key = self.path_key(key);
        let k = self.path_key(&self.expandvars(key));
        let sep = self.env_sep(&k);
        let expanded = value.expanded(&self.environ, &self.parent_environ);
        let rendered_value = if self.interpreter.expand_env_vars() {
            expanded.clone()
        } else {
            value.expanduser(
                self.environ
                    .get("HOME")
                    .or_else(|| self.parent_environ.get("HOME"))
                    .map(String::as_str),
            )
        };
        if !self.environ.contains_key(&k)
            && (self.all_parent_variables || self.parent_variables.contains(&k))
        {
            let parent = if cfg!(windows) && k == "PATH" {
                self.parent_get_path_value().cloned().unwrap_or_default()
            } else {
                self.parent_environ.get(&k).cloned().unwrap_or_default()
            };
            self.environ.insert(k.clone(), parent);
            self.interpreter
                .saferefenv(if self.interpreter.expand_env_vars() {
                    &k
                } else {
                    &raw_key
                });
        }
        if let Some(current) = self.environ.get(&k).cloned() {
            let combined = if prepend {
                format!("{expanded}{sep}{current}")
            } else {
                format!("{current}{sep}{expanded}")
            };
            let action = if prepend {
                Action::Prependenv {
                    key: raw_key.clone(),
                    value: value.clone(),
                }
            } else {
                Action::Appendenv {
                    key: raw_key.clone(),
                    value: value.clone(),
                }
            };
            self.actions.push(action);
            self.environ.insert(k.clone(), combined.clone());
            // Prior materialized data is opaque; a live shell reference reads runtime mutations.
            let reference = if self.interpreter.live_env() {
                RexValue::expandable(self.interpreter.key_token(&k))
            } else {
                RexValue::literal(current)
            };
            let mut rendered = if prepend {
                rendered_value.clone()
            } else {
                reference.clone()
            };
            rendered.append(RexValue::literal(sep));
            rendered.append(if prepend { reference } else { rendered_value });
            self.interpreter.apply_action(&Action::Setenv {
                key: if self.interpreter.expand_env_vars() {
                    k
                } else {
                    raw_key
                },
                value: rendered,
            });
        } else {
            self.setenv(key, value);
        }
    }

    pub fn alias(&mut self, name: &str, cmd: &str) {
        self.actions.push(Action::Alias {
            name: name.to_string(),
            cmd: cmd.to_string(),
        });
        self.interpreter.alias(name, cmd);
    }

    pub fn info(&mut self, msg: impl Into<RexValue>) {
        let msg = msg.into();
        self.actions.push(Action::Info { msg: msg.clone() });
        self.interpreter.apply_action(&Action::Info { msg });
    }

    pub fn error(&mut self, msg: impl Into<RexValue>) {
        let msg = msg.into();
        self.actions.push(Action::Error { msg: msg.clone() });
        self.interpreter.apply_action(&Action::Error { msg });
    }

    pub fn stop(&self, msg: &str) -> RezError {
        RezError::RexStop(msg.to_string())
    }

    pub fn command(&mut self, cmd: &str) {
        self.actions.push(Action::Command {
            cmd: cmd.to_string(),
        });
        self.interpreter.command(cmd);
    }

    pub fn command_args(&mut self, args: &[RexValue]) {
        let action = Action::CommandArgs {
            args: args.to_vec(),
        };
        self.actions.push(action.clone());
        self.interpreter.apply_action(&action);
    }

    pub fn comment(&mut self, text: &str) {
        self.actions.push(Action::Comment {
            text: text.to_string(),
        });
        self.interpreter.comment(text);
    }

    pub fn source(&mut self, path: impl Into<RexValue>) {
        let path = path.into();
        self.actions.push(Action::Source { path: path.clone() });
        self.interpreter.apply_action(&Action::Source { path });
    }

    pub fn shebang(&mut self) {
        self.actions.push(Action::Shebang { value: None });
        self.interpreter.shebang(None);
    }

    /// Returns the interpreter output.
    pub fn get_output(&self) -> &str {
        self.interpreter.get_output()
    }

    /// Restore a materialized current environment before replaying actions.
    /// Keys and values are already expanded; importing them must not run setenv.
    #[doc(hidden)]
    pub fn restore(&mut self, environ: HashMap<String, String>) {
        debug_assert!(self.actions.is_empty());
        debug_assert!(self.environ.is_empty());
        for (key, value) in environ {
            let key = self.path_key(&key);
            self.environ.insert(key.clone(), value.clone());
            self.interpreter.apply_action(&Action::Setenv {
                key,
                value: RexValue::literal(value),
            });
        }
    }

    /// Returns the current environment state.
    pub fn env(&self) -> &HashMap<String, String> {
        &self.environ
    }
}

// =============================================================================
// RexExecutor - high-level executor combining manager + bindings
// =============================================================================

/// High-level rex executor that runs actions against an interpreter.
///
/// Combines an ActionManager with namespace bindings for variables.
/// This is the main entry point for executing rex commands.
///
/// # Example
/// ```
/// use rex::{RexExecutor, OutputInterpreter};
///
/// let interp = OutputInterpreter::new();
/// let mut exec = RexExecutor::new(interp, None, true);
/// exec.setenv("FOO", "bar");
/// exec.appendenv("PATH", "/usr/local/bin");
/// let output = exec.get_output();
/// assert!(output.contains("export FOO=bar"));
/// ```
pub struct RexExecutor<I: ActionInterpreter> {
    manager: ActionManager<I>,
    /// Namespace bindings (variables available to rex code).
    bindings: HashMap<String, String>,
}

impl<I: ActionInterpreter> RexExecutor<I> {
    /// Create a new executor with the given interpreter.
    ///
    /// If `parent_environ` is None, the current process env is used.
    /// If `shebang` is true, a shebang is emitted at the start.
    pub fn new(
        interpreter: I,
        parent_environ: Option<HashMap<String, String>>,
        shebang: bool,
    ) -> Self {
        let mut manager = ActionManager::new(interpreter, parent_environ);
        if shebang {
            manager.shebang();
        }
        Self {
            manager,
            bindings: HashMap::new(),
        }
    }

    /// Bind a string variable into the executor namespace.
    pub fn bind(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.bindings.insert(name.into(), value.into());
    }

    /// Remove a binding from the namespace.
    pub fn unbind(&mut self, name: &str) {
        self.bindings.remove(name);
    }

    /// Get a namespace binding.
    pub fn get_binding(&self, name: &str) -> Option<&str> {
        self.bindings.get(name).map(|s| s.as_str())
    }

    // -- Forwarded action methods --

    pub fn setenv(&mut self, key: &str, value: impl Into<RexValue>) {
        self.manager.setenv(key, value);
    }

    pub fn unsetenv(&mut self, key: &str) {
        self.manager.unsetenv(key);
    }

    pub fn resetenv(
        &mut self,
        key: &str,
        value: impl Into<RexValue>,
        friends: Option<Vec<String>>,
    ) {
        self.manager.resetenv(key, value, friends);
    }

    pub fn prependenv(&mut self, key: &str, value: impl Into<RexValue>) {
        self.manager.prependenv(key, value);
    }

    pub fn appendenv(&mut self, key: &str, value: impl Into<RexValue>) {
        self.manager.appendenv(key, value);
    }

    pub fn alias(&mut self, name: &str, cmd: &str) {
        self.manager.alias(name, cmd);
    }

    pub fn info(&mut self, msg: impl Into<RexValue>) {
        self.manager.info(msg);
    }

    pub fn error(&mut self, msg: impl Into<RexValue>) {
        self.manager.error(msg);
    }

    pub fn stop(&self, msg: &str) -> RezError {
        self.manager.stop(msg)
    }

    pub fn command(&mut self, cmd: &str) {
        self.manager.command(cmd);
    }

    pub fn command_args(&mut self, args: &[RexValue]) {
        self.manager.command_args(args);
    }

    pub fn comment(&mut self, text: &str) {
        self.manager.comment(text);
    }

    pub fn source(&mut self, path: impl Into<RexValue>) {
        self.manager.source(path);
    }

    pub fn shebang(&mut self) {
        self.manager.shebang();
    }

    /// Check if an env var is defined.
    pub fn defined(&self, key: &str) -> bool {
        self.manager.defined(key)
    }

    /// Check if an env var is undefined.
    pub fn undefined(&self, key: &str) -> bool {
        self.manager.undefined(key)
    }

    /// Get env var value.
    pub fn getenv(&self, key: &str) -> Result<&str> {
        self.manager.getenv(key)
    }

    /// Get the recorded actions.
    pub fn actions(&self) -> &[Action] {
        &self.manager.actions
    }

    /// Get the interpreter output.
    pub fn get_output(&self) -> &str {
        self.manager.get_output()
    }

    /// Get the current environment state.
    pub fn env(&self) -> &HashMap<String, String> {
        self.manager.env()
    }

    /// Get a mutable reference to the manager.
    pub fn manager_mut(&mut self) -> &mut ActionManager<I> {
        &mut self.manager
    }

    /// Get a reference to the manager.
    pub fn manager(&self) -> &ActionManager<I> {
        &self.manager
    }

    /// Get a reference to the interpreter.
    pub fn interpreter(&self) -> &I {
        &self.manager.interpreter
    }

    /// Execute a sequence of pre-recorded actions through the interpreter.
    pub fn execute_actions(&mut self, actions: &[Action]) {
        for action in actions {
            match action {
                Action::Setenv { key, value } => self.setenv(key, value),
                Action::Unsetenv { key } => self.unsetenv(key),
                Action::Resetenv {
                    key,
                    value,
                    friends,
                } => {
                    self.resetenv(key, value, friends.clone());
                }
                Action::Prependenv { key, value } => self.prependenv(key, value),
                Action::Appendenv { key, value } => self.appendenv(key, value),
                Action::Alias { name, cmd } => self.alias(name, cmd),
                Action::Source { path } => self.source(path),
                Action::Command { cmd } => self.command(cmd),
                Action::CommandArgs { args } => self.command_args(args),
                Action::Info { msg } => self.info(msg),
                Action::Error { msg } => self.error(msg),
                Action::Comment { text } => self.comment(text),
                Action::Shebang { .. } => self.shebang(),
                Action::Stop { .. } => {} // Stop is handled via error return
            }
        }
    }
}

// =============================================================================
// EphemeralsDict - storage for ephemeral variables (_.varname pattern)
// =============================================================================

/// Storage for ephemeral variables used in rex code.
///
/// Ephemeral vars use the `_.varname` pattern and are stripped of the leading dot
/// when accessed. They represent transient request-time parameters.
#[derive(Clone, Debug, Default)]
pub struct EphemeralsDict {
    data: HashMap<String, String>,
}

impl EphemeralsDict {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create from a list of (name, value) pairs.
    /// Names should include the leading dot, which will be stripped for lookup.
    pub fn from_entries(entries: impl IntoIterator<Item = (String, String)>) -> Self {
        let data = entries
            .into_iter()
            .map(|(name, value)| {
                let key = if let Some(stripped) = name.strip_prefix('.') {
                    stripped.to_string()
                } else {
                    name
                };
                (key, value)
            })
            .collect();
        Self { data }
    }

    /// Get an ephemeral value by name (without leading dot).
    pub fn get(&self, name: &str) -> Option<&str> {
        self.data.get(name).map(|s| s.as_str())
    }

    /// Check if an ephemeral exists.
    pub fn contains(&self, name: &str) -> bool {
        self.data.contains_key(name)
    }

    /// Set an ephemeral value.
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.data.insert(name.into(), value.into());
    }

    /// Inner data reference.
    pub fn data(&self) -> &HashMap<String, String> {
        &self.data
    }
}

// =============================================================================
// VariantBinding - read-only access to variant attributes for rex code
// =============================================================================

/// Read-only binding to a package variant for use in rex code.
///
/// Exposes version, root, name, and other attributes that rex code can
/// reference as `this.root`, `this.version`, etc.
#[derive(Clone, Debug)]
pub struct VariantBinding {
    pub name: String,
    pub version: Version,
    pub root: Option<String>,
    pub base: Option<String>,
    /// Extra attributes (arbitrary k-v for extensibility).
    pub attrs: HashMap<String, String>,
}

impl VariantBinding {
    /// Create a new variant binding with core attributes.
    pub fn new(name: impl Into<String>, version: Version, root: Option<String>) -> Self {
        Self {
            name: name.into(),
            version,
            root,
            base: None,
            attrs: HashMap::new(),
        }
    }

    /// Get the qualified package name ("name-version").
    pub fn qualified_name(&self) -> String {
        format!("{}-{}", self.name, self.version)
    }

    /// Get an attribute by name, checking special fields first.
    pub fn get_attr(&self, attr: &str) -> Option<String> {
        match attr {
            "name" => Some(self.name.clone()),
            "version" => Some(self.version.to_string()),
            "root" => self.root.clone(),
            "base" => self.base.clone(),
            _ => self.attrs.get(attr).cloned(),
        }
    }

    /// Version major component.
    pub fn major(&self) -> Option<String> {
        self.version.get(0).map(|t| t.to_string())
    }

    /// Version minor component.
    pub fn minor(&self) -> Option<String> {
        self.version.get(1).map(|t| t.to_string())
    }

    /// Version patch component.
    pub fn patch(&self) -> Option<String> {
        self.version.get(2).map(|t| t.to_string())
    }
}

impl std::fmt::Display for VariantBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.qualified_name())
    }
}

// =============================================================================
// VersionBinding - read-only version access for rex code
// =============================================================================

/// Read-only binding to a Version for use in rex code.
///
/// Provides .major, .minor, .patch access and indexing.
#[derive(Clone, Debug)]
pub struct VersionBinding {
    version: Version,
}

impl VersionBinding {
    pub fn new(version: Version) -> Self {
        Self { version }
    }

    pub fn major(&self) -> Option<String> {
        self.version.get(0).map(|t| t.to_string())
    }

    pub fn minor(&self) -> Option<String> {
        self.version.get(1).map(|t| t.to_string())
    }

    pub fn patch(&self) -> Option<String> {
        self.version.get(2).map(|t| t.to_string())
    }

    pub fn get(&self, index: usize) -> Option<String> {
        self.version.get(index).map(|t| t.to_string())
    }

    pub fn len(&self) -> usize {
        self.version.len()
    }

    pub fn is_empty(&self) -> bool {
        self.version.is_empty()
    }

    /// Returns version tokens as a vec of strings.
    pub fn as_vec(&self) -> Vec<String> {
        (0..self.len())
            .filter_map(|i| self.version.get(i).map(|t| t.to_string()))
            .collect()
    }
}

impl std::fmt::Display for VersionBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.version)
    }
}

// =============================================================================
// RequirementsBinding - read-only requirements map for rex code
// =============================================================================

/// Read-only binding for a set of requirements, keyed by package name.
#[derive(Clone, Debug, Default)]
pub struct RequirementsBinding {
    data: HashMap<String, String>,
}

impl RequirementsBinding {
    /// Create from a list of requirement strings ("name-range").
    pub fn from_req_strings(reqs: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            data: reqs.into_iter().collect(),
        }
    }

    /// Get the requirement string for a package name.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.data.get(name).map(|s| s.as_str())
    }

    /// Check if a requirement exists.
    pub fn contains(&self, name: &str) -> bool {
        self.data.contains_key(name)
    }

    pub fn data(&self) -> &HashMap<String, String> {
        &self.data
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    #[test]
    fn test_rex_value_text_equality_preserves_segmented_semantics() {
        let mut value = RexValue::literal("");
        value.append(RexValue::expandable("é"));
        value.append(RexValue::literal("水"));
        value.append(RexValue::expandable(""));
        value.append(RexValue::literal("😀"));
        assert_eq!(value, "é水😀");
        assert!(<RexValue as PartialEq<str>>::eq(&value, "é水😀"));
        assert_ne!(value, "é水");
        assert_ne!(value, "é水😀x");
        assert_ne!(value, "e水😀");
        assert_ne!(value, RexValue::literal("é水😀"));
        assert_eq!(RexValue(vec![]), "");
        assert_eq!(RexValue::literal(""), "");
        assert_ne!(RexValue(vec![]), "x");
    }

    #[test]
    fn test_rex_value_preserves_literal_and_mixed_expansion() {
        let mut value = RexValue::literal("${USER} ");
        value.append(RexValue::expandable("${USER}/$BASE"));
        value.append(RexValue::literal(" ${USER}"));
        let parent = HashMap::from([
            ("USER".into(), "alice".into()),
            ("BASE".into(), "base".into()),
        ]);
        assert_eq!(
            value.expanded(&HashMap::new(), &parent),
            "${USER} alice/base ${USER}"
        );
        assert_eq!(value.0.len(), 3);
        let expanded = value.expanded(&HashMap::new(), &parent);
        assert!(expanded.0[0].literal);
        assert!(!expanded.0[1].literal);
        assert!(expanded.0[2].literal);
        assert_eq!(expanded.0[1].text, "/");
        assert_eq!(
            RexValue::expandable("$UNKNOWN").expanded(&HashMap::new(), &parent),
            RexValue::expandable("$UNKNOWN")
        );
        let mut manager = ActionManager::new(PythonInterpreter::new(), Some(parent));
        manager.setenv("VALUE", &value);
        assert_eq!(
            manager.getenv("VALUE").unwrap(),
            "${USER} alice/base ${USER}"
        );
        assert_eq!(
            manager.actions[0],
            Action::Setenv {
                key: "VALUE".into(),
                value
            }
        );
    }

    #[test]
    fn test_rex_value_two_passes_preserve_substitution_origin() {
        let current = HashMap::from([("A".into(), "$".into())]);
        let parent = HashMap::from([
            ("B".into(), "水😀$UNKNOWN".into()),
            ("HOME".into(), "C:/%HOME_LOOKALIKE%/$HOME_LOOKALIKE".into()),
        ]);
        let expanded = RexValue::expandable("${A}B/$UNRESOLVED").expanded(&current, &parent);
        assert_eq!(expanded.to_string(), "水😀$UNKNOWN/$UNRESOLVED");
        let mut expected = RexValue::literal("水😀$UNKNOWN");
        expected.append(RexValue::expandable("/$UNRESOLVED"));
        assert_eq!(expanded, expected);
        let current = HashMap::from([("A".into(), "pre-$B-post".into())]);
        assert_eq!(
            RexValue::expandable("$A").expanded(&current, &parent),
            RexValue::literal("pre-水😀$UNKNOWN-post")
        );
        let mut expected = RexValue::literal("C:/%HOME_LOOKALIKE%/$HOME_LOOKALIKE");
        expected.append(RexValue::expandable("/$UNRESOLVED"));
        assert_eq!(
            RexValue::expandable("~/$UNRESOLVED").expanded(&HashMap::new(), &parent),
            expected
        );
        assert_eq!(
            RexValue::literal("~/$B").expanded(&current, &parent),
            RexValue::literal("~/$B")
        );
        assert_eq!(
            RexValue::literal("").expanded(&current, &parent),
            RexValue::literal("")
        );
        assert_eq!(
            RexValue::expandable("$EMPTY")
                .expanded(
                    &HashMap::new(),
                    &HashMap::from([("EMPTY".into(), String::new())])
                )
                .to_string(),
            ""
        );
    }

    #[test]
    fn test_rex_value_concatenation_merges_only_matching_provenance() {
        let mut value = RexValue::literal("a");
        value.append(RexValue::literal("b"));
        value.append("c");
        value.append("d");
        assert_eq!(
            value.0,
            vec![
                RexSegment {
                    literal: true,
                    text: "ab".into()
                },
                RexSegment {
                    literal: false,
                    text: "cd".into()
                },
            ]
        );
        assert_eq!(value.formatted(|s| s.to_uppercase()).to_string(), "abCD");
        assert_eq!(
            serde_json::from_str::<RexValue>(&serde_json::to_string(&value).unwrap()).unwrap(),
            value
        );
        assert!(serde_json::from_str::<RexValue>(r#"[{"literal":"yes","text":"bad"}]"#).is_err());
    }

    #[test]
    fn test_rex_value_prepend_parent_is_literal_and_separator_is_preserved() {
        let mut manager = ActionManager::new(
            OutputInterpreter::new(),
            Some(HashMap::from([
                ("VALUE".into(), "$UNCHANGED".into()),
                ("NEW".into(), "expanded".into()),
            ])),
        );
        manager.parent_variables.insert("VALUE".into());
        manager.set_env_sep("VALUE", "|");
        manager.prependenv("VALUE", RexValue::expandable("$NEW"));
        assert_eq!(manager.getenv("VALUE").unwrap(), "expanded|$UNCHANGED");
        match &manager.actions[0] {
            Action::Prependenv { key, value } => {
                assert_eq!(key, "VALUE");
                assert_eq!(value, &RexValue::expandable("$NEW"));
                assert_eq!(manager.get_output(), "export VALUE=$NEW|${VALUE}");
            }
            other => panic!("unexpected action {other:?}"),
        }
        manager.appendenv("VALUE", RexValue::literal("$NEW"));
        assert_eq!(manager.getenv("VALUE").unwrap(), "expanded|$UNCHANGED|$NEW");
    }

    use super::*;

    #[test]
    fn test_rex_expanded_keys_empty_values_and_query_replay() {
        for prepend in [false, true] {
            let mut manager = ActionManager::new(PythonInterpreter::new(), None);
            manager.set_env_sep("VALUE", "|");
            manager.parent_variables.insert("VALUE".into());
            if prepend {
                manager.prependenv("VALUE", "x");
            } else {
                manager.appendenv("VALUE", "x");
            }
            assert_eq!(
                manager.getenv("VALUE").unwrap(),
                if prepend { "x|" } else { "|x" }
            );
            manager.setenv("VALUE", "");
            if prepend {
                manager.prependenv("VALUE", "x");
            } else {
                manager.appendenv("VALUE", "x");
            }
            assert_eq!(
                manager.getenv("VALUE").unwrap(),
                if prepend { "x|" } else { "|x" }
            );
        }
        let actions = serde_json::json!([
            {"t": "set", "k": "NAME", "v": "FOO"},
            {"t": "append", "k": "${NAME}", "v": "a"},
            {"t": "append", "k": "${NAME}", "v": "b"}
        ]);
        let state = serde_json::json!({
            "parent": {}, "current": {}, "separators": {"FOO": ","}, "separator": ":",
            "keys": ["FOO"]
        });
        let environment = crate::rex_environ(&actions, &state, None).unwrap();
        assert_eq!(environment["FOO"], "a,b");
        let query = crate::rex_environ(
            &actions,
            &state,
            Some(serde_json::to_value(RexValue::expandable("${NAME}:${FOO}")).unwrap()),
        )
        .unwrap();
        assert_eq!(query, "FOO:a,b");
        let state = serde_json::json!({
            "parent": {"A": "$B", "B": "TARGET"},
            "current": {"$B": "materialized"},
            "separators": {}, "separator": ":"
        });
        let query = crate::rex_environ(
            &serde_json::json!([]),
            &state,
            Some(serde_json::json!({"operation": "getenv", "key": "$A"})),
        )
        .unwrap();
        assert_eq!(query, "materialized");
        let snapshot = crate::rex_environ(&serde_json::json!([]), &state, None).unwrap();
        assert_eq!(snapshot["$B"], "materialized");
        assert!(snapshot.get("TARGET").is_none());
        let mut manager = ActionManager::new(OutputInterpreter::new(), None);
        manager.setenv("NAME", "FOO");
        manager.appendenv("${NAME}", "a");
        manager.appendenv("${NAME}", "b");
        assert_eq!(manager.actions.last().unwrap().env_key(), Some("${NAME}"));
        assert!(manager.get_output().contains("${FOO}"));
        assert!(!manager.get_output().contains("${${NAME}}"));
    }

    // -- Action tests --

    #[test]
    fn test_action_name() {
        let a = Action::Setenv {
            key: "FOO".into(),
            value: "bar".into(),
        };
        assert_eq!(a.name(), "setenv");

        let b = Action::Unsetenv { key: "FOO".into() };
        assert_eq!(b.name(), "unsetenv");

        let c = Action::Stop { msg: "done".into() };
        assert_eq!(c.name(), "stop");
    }

    #[test]
    fn test_action_env_key() {
        let a = Action::Setenv {
            key: "PATH".into(),
            value: "/bin".into(),
        };
        assert_eq!(a.env_key(), Some("PATH"));

        let b = Action::Comment {
            text: "hello".into(),
        };
        assert_eq!(b.env_key(), None);
    }

    #[test]
    fn test_action_display() {
        let a = Action::Setenv {
            key: "X".into(),
            value: "1".into(),
        };
        assert_eq!(format!("{a}"), "setenv(X, 1)");
    }

    #[test]
    fn test_action_equality() {
        let a1 = Action::Setenv {
            key: "A".into(),
            value: "1".into(),
        };
        let a2 = Action::Setenv {
            key: "A".into(),
            value: "1".into(),
        };
        let a3 = Action::Setenv {
            key: "A".into(),
            value: "2".into(),
        };
        assert_eq!(a1, a2);
        assert_ne!(a1, a3);
    }

    // -- EnvironmentDict tests --

    #[test]
    fn test_env_dict_basic() {
        let mut env =
            EnvironmentDict::new(Some(HashMap::from([("HOME".into(), "/home/user".into())])));

        assert_eq!(env.get("HOME"), Some("/home/user"));
        assert_eq!(env.get("NONEXIST"), None);
        assert!(env.contains("HOME"));
        assert!(!env.contains("NONEXIST"));

        env.set("FOO", "bar");
        assert_eq!(env.get("FOO"), Some("bar"));

        env.unset("FOO");
        assert_eq!(env.get("FOO"), None);
    }

    #[test]
    fn test_env_dict_override_parent() {
        let mut env =
            EnvironmentDict::new(Some(HashMap::from([("KEY".into(), "parent_val".into())])));

        // Parent value visible
        assert_eq!(env.get("KEY"), Some("parent_val"));

        // Override takes precedence
        env.set("KEY", "override_val");
        assert_eq!(env.get("KEY"), Some("override_val"));

        // Removing override reveals parent again
        env.unset("KEY");
        assert_eq!(env.get("KEY"), Some("parent_val"));
    }

    #[test]
    fn test_env_dict_append_prepend() {
        let mut env = EnvironmentDict::new(Some(HashMap::new()));

        // Append to empty key
        env.append("PATH", "/usr/bin", ":");
        assert_eq!(env.get("PATH"), Some("/usr/bin"));

        // Append to existing
        env.append("PATH", "/usr/local/bin", ":");
        assert_eq!(env.get("PATH"), Some("/usr/bin:/usr/local/bin"));

        // Prepend
        env.prepend("PATH", "/opt/bin", ":");
        assert_eq!(env.get("PATH"), Some("/opt/bin:/usr/bin:/usr/local/bin"));
    }

    #[test]
    fn test_env_dict_merged() {
        let mut env = EnvironmentDict::new(Some(HashMap::from([
            ("A".into(), "1".into()),
            ("B".into(), "2".into()),
        ])));
        env.set("B", "99");
        env.set("C", "3");

        let merged = env.merged();
        assert_eq!(merged.get("A"), Some(&"1".to_string()));
        assert_eq!(merged.get("B"), Some(&"99".to_string()));
        assert_eq!(merged.get("C"), Some(&"3".to_string()));
    }

    // -- OutputInterpreter tests --

    #[test]
    fn test_output_interp_setenv() {
        let mut interp = OutputInterpreter::new();
        interp.setenv("FOO", "bar");
        assert_eq!(interp.get_output(), "export FOO=bar");
    }

    #[test]
    fn test_output_interp_unsetenv() {
        let mut interp = OutputInterpreter::new();
        interp.unsetenv("FOO");
        assert_eq!(interp.get_output(), "unset FOO");
    }

    #[test]
    fn test_output_interp_alias() {
        let mut interp = OutputInterpreter::new();
        interp.alias("ll", "ls -la");
        assert_eq!(interp.get_output(), "alias ll='ls -la'");
    }

    #[test]
    fn test_output_interp_comment() {
        let mut interp = OutputInterpreter::new();
        interp.comment("this is a comment");
        assert_eq!(interp.get_output(), "# this is a comment");
    }

    #[test]
    fn test_output_interp_multiple() {
        let mut interp = OutputInterpreter::new();
        interp.setenv("A", "1");
        interp.setenv("B", "2");
        interp.comment("done");
        let output = interp.get_output();
        assert!(output.contains("export A=1"));
        assert!(output.contains("export B=2"));
        assert!(output.contains("# done"));
    }

    #[test]
    fn test_output_interp_source() {
        let mut interp = OutputInterpreter::new();
        interp.source("/etc/profile");
        assert_eq!(interp.get_output(), "source /etc/profile");
    }

    // -- PythonInterpreter tests --

    #[test]
    fn test_python_interp_setenv() {
        let mut interp = PythonInterpreter::new();
        interp.setenv("X", "1");
        assert_eq!(interp.env().get("X"), Some(&"1".to_string()));
    }

    #[test]
    fn test_python_interp_unsetenv() {
        let mut interp = PythonInterpreter::new();
        interp.setenv("X", "1");
        interp.unsetenv("X");
        assert_eq!(interp.env().get("X"), None);
    }

    #[test]
    fn test_python_interp_prepend_append() {
        let mut interp = PythonInterpreter::new();
        interp.setenv("PATH", "/usr/bin");
        interp.prependenv("PATH", "/opt/bin");
        interp.appendenv("PATH", "/usr/local/bin");

        let sep = path_sep();
        let expected = format!("/opt/bin{sep}/usr/bin{sep}/usr/local/bin");
        assert_eq!(interp.env().get("PATH"), Some(&expected));
    }

    // -- ActionManager tests --

    #[test]
    fn test_manager_setenv_records_action() {
        let interp = OutputInterpreter::new();
        let mut mgr = ActionManager::new(interp, Some(HashMap::new()));

        mgr.setenv("FOO", "bar");

        assert_eq!(mgr.actions.len(), 1);
        assert_eq!(
            mgr.actions[0],
            Action::Setenv {
                key: "FOO".into(),
                value: "bar".into()
            }
        );
        assert_eq!(mgr.env().get("FOO"), Some(&"bar".to_string()));
    }

    #[test]
    fn test_manager_unsetenv() {
        let interp = OutputInterpreter::new();
        let mut mgr = ActionManager::new(interp, Some(HashMap::new()));

        mgr.setenv("FOO", "bar");
        assert!(mgr.defined("FOO"));

        mgr.unsetenv("FOO");
        assert!(mgr.undefined("FOO"));
        assert_eq!(mgr.actions.len(), 2);
    }

    #[test]
    fn test_manager_prependenv_first_ref_is_setenv() {
        let interp = OutputInterpreter::new();
        let mut mgr = ActionManager::new(interp, Some(HashMap::new()));

        // First reference to PATH -> should become setenv (Path on Windows for canonical key)
        mgr.prependenv("PATH", "/usr/bin");
        assert_eq!(mgr.actions.len(), 1);
        let path_key = "PATH";
        assert_eq!(
            mgr.actions[0],
            Action::Setenv {
                key: path_key.into(),
                value: "/usr/bin".into()
            }
        );
    }

    #[test]
    fn test_manager_prependenv_subsequent_is_prepend() {
        let interp = OutputInterpreter::new();
        let mut mgr = ActionManager::new(interp, Some(HashMap::new()));

        mgr.setenv("PATH", "/usr/bin");
        mgr.prependenv("PATH", "/opt/bin");

        assert_eq!(mgr.actions.len(), 2);
        let path_key = "PATH";
        assert_eq!(
            mgr.actions[1],
            Action::Prependenv {
                key: path_key.into(),
                value: "/opt/bin".into()
            }
        );
    }

    #[test]
    fn test_manager_appendenv() {
        let interp = OutputInterpreter::new();
        let mut mgr = ActionManager::new(interp, Some(HashMap::new()));

        mgr.setenv("PATH", "/usr/bin");
        mgr.appendenv("PATH", "/opt/bin");

        assert_eq!(mgr.actions.len(), 2);
        let path_key = "PATH";
        assert_eq!(
            mgr.actions[1],
            Action::Appendenv {
                key: path_key.into(),
                value: "/opt/bin".into()
            }
        );

        let sep = path_sep();
        assert_eq!(
            mgr.env().get(path_key),
            Some(&format!("/usr/bin{sep}/opt/bin"))
        );
    }

    #[test]
    fn test_manager_getenv() {
        let parent = HashMap::from([("HOME".into(), "/home/user".into())]);
        let interp = OutputInterpreter::new();
        let mgr = ActionManager::new(interp, Some(parent));

        assert_eq!(mgr.getenv("HOME").unwrap(), "/home/user");
        assert!(mgr.getenv("NOPE").is_err());
    }

    #[test]
    fn test_manager_defined_undefined() {
        let parent = HashMap::from([("HOME".into(), "/home".into())]);
        let interp = OutputInterpreter::new();
        let mgr = ActionManager::new(interp, Some(parent));

        assert!(mgr.defined("HOME"));
        assert!(mgr.undefined("NOPE"));
    }

    #[test]
    fn test_manager_stop() {
        let interp = OutputInterpreter::new();
        let mgr = ActionManager::new(interp, Some(HashMap::new()));

        let err = mgr.stop("halted");
        match err {
            RezError::RexStop(msg) => assert_eq!(msg, "halted"),
            _ => panic!("Expected RexStop"),
        }
    }

    #[test]
    fn test_manager_alias_comment_source() {
        let interp = OutputInterpreter::new();
        let mut mgr = ActionManager::new(interp, Some(HashMap::new()));

        mgr.alias("ll", "ls -la");
        mgr.comment("hello");
        mgr.source("/etc/profile");

        assert_eq!(mgr.actions.len(), 3);
        assert_eq!(mgr.actions[0].name(), "alias");
        assert_eq!(mgr.actions[1].name(), "comment");
        assert_eq!(mgr.actions[2].name(), "source");
    }

    // -- RexExecutor tests --

    #[test]
    fn test_executor_basic() {
        let interp = OutputInterpreter::new();
        let mut exec = RexExecutor::new(interp, Some(HashMap::new()), false);

        exec.setenv("FOO", "bar");
        exec.appendenv("PATH", "/usr/bin");
        exec.comment("done");

        assert_eq!(exec.actions().len(), 3);
        let output = exec.get_output();
        assert!(output.contains("export FOO=bar"));
    }

    #[test]
    fn test_executor_with_shebang() {
        let interp = OutputInterpreter::new();
        let mut exec = RexExecutor::new(interp, Some(HashMap::new()), true);

        exec.setenv("X", "1");
        let output = exec.get_output();
        assert!(output.starts_with("#!/bin/bash"));
    }

    #[test]
    fn test_executor_bindings() {
        let interp = OutputInterpreter::new();
        let mut exec = RexExecutor::new(interp, Some(HashMap::new()), false);

        exec.bind("root", "/packages/foo/1.0");
        assert_eq!(exec.get_binding("root"), Some("/packages/foo/1.0"));

        exec.unbind("root");
        assert_eq!(exec.get_binding("root"), None);
    }

    #[test]
    fn test_executor_env_query() {
        let parent = HashMap::from([("HOME".into(), "/home/test".into())]);
        let interp = OutputInterpreter::new();
        let exec = RexExecutor::new(interp, Some(parent), false);

        assert!(exec.defined("HOME"));
        assert!(exec.undefined("NOPE"));
        assert_eq!(exec.getenv("HOME").unwrap(), "/home/test");
    }

    #[test]
    fn test_executor_execute_actions() {
        let actions = vec![
            Action::Setenv {
                key: "A".into(),
                value: "1".into(),
            },
            Action::Setenv {
                key: "B".into(),
                value: "2".into(),
            },
            Action::Comment {
                text: "test".into(),
            },
        ];

        let interp = OutputInterpreter::new();
        let mut exec = RexExecutor::new(interp, Some(HashMap::new()), false);
        exec.execute_actions(&actions);

        assert_eq!(exec.actions().len(), 3);
        assert_eq!(exec.env().get("A"), Some(&"1".to_string()));
        assert_eq!(exec.env().get("B"), Some(&"2".to_string()));
    }

    #[test]
    fn test_executor_python_interp() {
        let interp = PythonInterpreter::new();
        let mut exec = RexExecutor::new(interp, Some(HashMap::new()), false);

        exec.setenv("FOO", "bar");
        exec.setenv("PATH", "/usr/bin");
        exec.appendenv("PATH", "/opt/bin");

        let env = exec.env();
        assert_eq!(env.get("FOO"), Some(&"bar".to_string()));

        let path_key = "PATH";
        let sep = path_sep();
        assert_eq!(env.get(path_key), Some(&format!("/usr/bin{sep}/opt/bin")));
    }

    // -- EphemeralsDict tests --

    #[test]
    fn test_ephemerals_basic() {
        let eph = EphemeralsDict::from_entries(vec![
            (".foo.cli".into(), ".foo.cli-1".into()),
            (".bar.mode".into(), ".bar.mode-0".into()),
        ]);

        assert!(eph.contains("foo.cli"));
        assert!(eph.contains("bar.mode"));
        assert!(!eph.contains(".foo.cli")); // Leading dot stripped
        assert_eq!(eph.get("foo.cli"), Some(".foo.cli-1"));
    }

    #[test]
    fn test_ephemerals_set() {
        let mut eph = EphemeralsDict::new();
        eph.set("test.key", "value");
        assert_eq!(eph.get("test.key"), Some("value"));
    }

    // -- VariantBinding tests --

    #[test]
    fn test_variant_binding_basic() {
        let v = Version::new("1.2.3").unwrap();
        let binding = VariantBinding::new("maya", v, Some("/packages/maya/1.2.3".into()));

        assert_eq!(binding.name, "maya");
        assert_eq!(binding.qualified_name(), "maya-1.2.3");
        assert_eq!(binding.root, Some("/packages/maya/1.2.3".to_string()));
        assert_eq!(binding.major(), Some("1".to_string()));
        assert_eq!(binding.minor(), Some("2".to_string()));
        assert_eq!(binding.patch(), Some("3".to_string()));
    }

    #[test]
    fn test_variant_binding_get_attr() {
        let v = Version::new("2.0").unwrap();
        let mut binding = VariantBinding::new("houdini", v, None);
        binding.attrs.insert("tools".into(), "hython".into());

        assert_eq!(binding.get_attr("name"), Some("houdini".to_string()));
        assert_eq!(binding.get_attr("version"), Some("2.0".to_string()));
        assert_eq!(binding.get_attr("tools"), Some("hython".to_string()));
        assert_eq!(binding.get_attr("nope"), None);
    }

    #[test]
    fn test_variant_binding_display() {
        let v = Version::new("3.1.4").unwrap();
        let binding = VariantBinding::new("pkg", v, None);
        assert_eq!(format!("{binding}"), "pkg-3.1.4");
    }

    // -- VersionBinding tests --

    #[test]
    fn test_version_binding() {
        let v = Version::new("1.2.3alpha").unwrap();
        let binding = VersionBinding::new(v);

        assert_eq!(binding.major(), Some("1".to_string()));
        assert_eq!(binding.minor(), Some("2".to_string()));
        assert_eq!(binding.patch(), Some("3alpha".to_string()));
        assert_eq!(binding.len(), 3);
        assert_eq!(format!("{binding}"), "1.2.3alpha");
    }

    #[test]
    fn test_version_binding_out_of_range() {
        let v = Version::new("1.0").unwrap();
        let binding = VersionBinding::new(v);

        assert_eq!(binding.get(0), Some("1".to_string()));
        assert_eq!(binding.get(1), Some("0".to_string()));
        assert_eq!(binding.get(5), None);
    }

    #[test]
    fn test_version_binding_as_vec() {
        let v = Version::new("1.2.3").unwrap();
        let binding = VersionBinding::new(v);
        assert_eq!(binding.as_vec(), vec!["1", "2", "3"]);
    }

    // -- RequirementsBinding tests --

    #[test]
    fn test_requirements_binding() {
        let reqs = RequirementsBinding::from_req_strings(vec![
            ("maya".into(), "maya-2024+".into()),
            ("python".into(), "python-3.9+<3.12".into()),
        ]);

        assert!(reqs.contains("maya"));
        assert!(reqs.contains("python"));
        assert!(!reqs.contains("houdini"));
        assert_eq!(reqs.get("maya"), Some("maya-2024+"));
    }

    // -- Integration tests --

    #[test]
    fn test_full_rex_session() {
        let interp = OutputInterpreter::new();
        let mut exec = RexExecutor::new(interp, Some(HashMap::new()), true);

        // Set up a package environment
        exec.comment("Setting up maya-2024");
        exec.setenv("REZ_MAYA_VERSION", "2024");
        exec.setenv("REZ_MAYA_ROOT", "/packages/maya/2024");
        exec.setenv("PATH", "/packages/maya/2024/bin");
        exec.appendenv("PATH", "/usr/local/bin");
        exec.prependenv("PATH", "/opt/bin");
        exec.alias("maya", "maya2024");
        exec.info("Maya 2024 loaded");

        // Verify actions
        let actions = exec.actions();
        assert_eq!(actions.len(), 9); // comment + 3 setenv + append + prepend + alias + info + shebang(if any) = 8-9

        // Verify env state
        assert!(exec.defined("REZ_MAYA_VERSION"));
        assert_eq!(exec.getenv("REZ_MAYA_VERSION").unwrap(), "2024");

        let sep = path_sep();
        assert_eq!(
            exec.getenv("PATH").unwrap(),
            format!("/opt/bin{sep}/packages/maya/2024/bin{sep}/usr/local/bin")
        );

        // Verify output
        let output = exec.get_output();
        assert!(output.starts_with("#!/bin/bash"));
        assert!(output.contains("export REZ_MAYA_VERSION=2024"));
        assert!(output.contains("alias maya='maya2024'"));
        assert!(output.contains("echo Maya 2024 loaded"));
    }

    #[test]
    fn test_resetenv_with_friends() {
        let interp = OutputInterpreter::new();
        let mut exec = RexExecutor::new(interp, Some(HashMap::new()), false);

        exec.resetenv(
            "MAYA_LOCATION",
            "/opt/maya",
            Some(vec!["MAYA_VERSION".into()]),
        );

        assert_eq!(exec.actions().len(), 1);
        match &exec.actions()[0] {
            Action::Resetenv {
                key,
                value,
                friends,
            } => {
                assert_eq!(key, "MAYA_LOCATION");
                assert_eq!(value, "/opt/maya");
                assert_eq!(friends.as_ref().unwrap(), &vec!["MAYA_VERSION".to_string()]);
            }
            _ => panic!("Expected Resetenv"),
        }
    }

    #[test]
    fn test_env_sep_override() {
        let interp = OutputInterpreter::new();
        let mut mgr = ActionManager::new(interp, Some(HashMap::new()));

        mgr.set_env_sep("PYTHONPATH", ";");
        mgr.setenv("PYTHONPATH", "/lib/python");
        mgr.appendenv("PYTHONPATH", "/extra/python");

        assert_eq!(
            mgr.env().get("PYTHONPATH"),
            Some(&"/lib/python;/extra/python".to_string())
        );
    }
}
