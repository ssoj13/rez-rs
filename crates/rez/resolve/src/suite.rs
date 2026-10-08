//! Suite management - collections of contexts with tool wrappers.
//!
//! Ported from Python rez suite.py. Manages context collections with unified tool access.
//     suite.yaml          - suite metadata (contexts, tool config) - Python rez format
//     suite.json          - legacy format (still supported for loading)
//     contexts/
//       <name>.rxt        - serialized resolved context files
//     bin/
//       <tool_alias>      - generated wrapper scripts

// Forward reference - resolved_context.rs provides:
// crate::resolve::context::ResolvedContext - main resolved environment struct
// We don't import it directly to avoid compilation dependency.
// Instead, store context paths and load on demand.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::constants::SuiteVisibility;
use crate::errors::{Result, RezError};

// ---------------------------------------------------------------------------
// SuiteContext - per-context configuration within a suite
// ---------------------------------------------------------------------------

/// Configuration for a single context within a suite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteContext {
    /// Name of this context in the suite
    pub name: String,
    /// Path to the .rxt context file
    pub context_path: PathBuf,
    /// Tool name prefix (e.g. "maya_" makes "render" -> "maya_render")
    #[serde(default)]
    pub prefix: String,
    /// Tool name suffix (e.g. "_2024" makes "render" -> "render_2024")
    #[serde(default)]
    pub suffix: String,
    /// Priority for tool conflict resolution (higher wins)
    pub priority: i32,
    /// Tool visibility setting for this context
    #[serde(default)]
    pub visibility: SuiteVisibility,
    /// Explicit tool aliases: original_name -> alias_name
    #[serde(default)]
    pub tool_aliases: HashMap<String, String>,
    /// Set of hidden tool names
    #[serde(default)]
    pub hidden_tools: HashSet<String>,
    /// Raw tool names available in this context (populated on add)
    #[serde(default)]
    pub tools: Vec<String>,
    /// Optional prefix char for forwarding scripts
    #[serde(default)]
    pub prefix_char: Option<String>,
}

// ---------------------------------------------------------------------------
// SuiteTool - describes a single resolved tool entry
// ---------------------------------------------------------------------------

/// A resolved tool entry exposed by the suite.
#[derive(Debug, Clone)]
pub struct SuiteTool {
    /// Original tool name from the context
    pub tool_name: String,
    /// Aliased name (after prefix/suffix/alias applied)
    pub tool_alias: String,
    /// Which context provides this tool
    pub context_name: String,
    /// Package name providing the tool (informational)
    pub package: Option<String>,
}

impl fmt::Display for SuiteTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} -> {} ({})",
            self.tool_alias, self.tool_name, self.context_name
        )
    }
}

// ---------------------------------------------------------------------------
// Cached tool resolution state
// ---------------------------------------------------------------------------

/// Internal cached tool resolution result.
#[derive(Debug, Clone, Default)]
struct ToolCache {
    /// Visible tools keyed by alias
    tools: HashMap<String, SuiteTool>,
    /// Tools hidden via hide_tool()
    hidden: Vec<SuiteTool>,
    /// Conflicting tools keyed by alias
    conflicts: HashMap<String, Vec<SuiteTool>>,
}

// ---------------------------------------------------------------------------
// Suite - main struct
// ---------------------------------------------------------------------------

/// Serializable suite data for suite.yaml (or legacy suite.json).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SuiteData {
    contexts: HashMap<String, SuiteContext>,
}

/// A collection of resolved contexts with managed tool wrappers.
///
/// Contexts are stored as .rxt files in the suite directory. Each context's
/// tools are exposed through wrapper scripts in a bin/ directory. Tool conflicts
/// are resolved by priority - the most recently added/bumped context wins.
#[derive(Debug)]
pub struct Suite {
    /// Path this suite was loaded from (None if not yet saved)
    pub load_path: Option<PathBuf>,
    /// Optional description
    pub description: Option<String>,
    /// Named contexts in the suite
    contexts: HashMap<String, SuiteContext>,
    /// Next priority counter (auto-incrementing)
    next_priority: i32,
    /// Cached tool resolution (None = needs recompute)
    cache: Option<ToolCache>,
}

impl Suite {
    // -- Construction --

    /// Create an empty suite.
    pub fn new() -> Self {
        Self {
            load_path: None,
            description: None,
            contexts: HashMap::new(),
            next_priority: 1,
            cache: None,
        }
    }

    /// Load a suite from a directory.
    /// Tries suite.yaml first (Python rez format), falls back to suite.json.
    pub fn load(path: &Path) -> Result<Self> {
        // Try YAML first (Python rez compatibility)
        let suite_yaml = path.join("suite.yaml");
        let suite_json = path.join("suite.json");

        let data: SuiteData = if suite_yaml.is_file() {
            let content = std::fs::read_to_string(&suite_yaml)?;
            serde_yaml::from_str(&content)
                .map_err(|e| RezError::Suite(format!("Failed loading suite.yaml: {e}")))?
        } else if suite_json.is_file() {
            let content = std::fs::read_to_string(&suite_json)?;
            serde_json::from_str(&content)
                .map_err(|e| RezError::Suite(format!("Failed loading suite.json: {e}")))?
        } else {
            return Err(RezError::Suite(format!("Not a suite: {}", path.display())));
        };

        let next_priority = if data.contexts.is_empty() {
            1
        } else {
            data.contexts
                .values()
                .map(|c| c.priority)
                .max()
                .unwrap_or(0)
                + 1
        };

        Ok(Self {
            load_path: Some(std::fs::canonicalize(path)?),
            description: None,
            contexts: data.contexts,
            next_priority,
            cache: None,
        })
    }

    /// Save the suite to a directory.
    pub fn save(&self, path: &Path) -> Result<()> {
        let path = if path.exists() {
            // If overwriting same suite, allow it
            if let Some(ref lp) = self.load_path {
                let canon = std::fs::canonicalize(path)?;
                if *lp != canon {
                    return Err(RezError::Suite(format!(
                        "Cannot save, path exists: {}",
                        path.display()
                    )));
                }
                // Remove old suite to rewrite
                std::fs::remove_dir_all(&canon)?;
            } else {
                return Err(RezError::Suite(format!(
                    "Cannot save, path exists: {}",
                    path.display()
                )));
            }
            std::fs::canonicalize(path.parent().unwrap_or(Path::new(".")))?
                .join(path.file_name().unwrap_or_default())
        } else {
            path.to_path_buf()
        };

        // Create directories
        let contexts_dir = path.join("contexts");
        std::fs::create_dir_all(&contexts_dir)?;

        // Write suite.yaml (Python rez format)
        let data = SuiteData {
            contexts: self.contexts.clone(),
        };
        let yaml = serde_yaml::to_string(&data)
            .map_err(|e| RezError::Suite(format!("Failed serializing suite: {e}")))?;
        std::fs::write(path.join("suite.yaml"), yaml)?;

        // Copy context .rxt files into contexts/ dir
        for (name, ctx) in &self.contexts {
            let dest = contexts_dir.join(format!("{name}.rxt"));
            if ctx.context_path.exists() && ctx.context_path != dest {
                std::fs::copy(&ctx.context_path, &dest)?;
            }
        }

        // Generate tool wrappers
        let bin_dir = path.join("bin");
        std::fs::create_dir_all(&bin_dir)?;
        self.generate_wrappers(&bin_dir, &path)?;

        Ok(())
    }

    // -- Context management --

    /// Add a context to the suite.
    ///
    /// `name` - unique name for this context in the suite
    /// `context_path` - path to the .rxt context file
    /// `tools` - list of tool names available in this context
    pub fn add_context(
        &mut self,
        name: &str,
        context_path: &Path,
        tools: Vec<String>,
    ) -> Result<()> {
        if self.contexts.contains_key(name) {
            return Err(RezError::Suite(format!(
                "Context already in suite: {name:?}"
            )));
        }
        let priority = self.next_priority();
        self.contexts.insert(
            name.to_string(),
            SuiteContext {
                name: name.to_string(),
                context_path: context_path.to_path_buf(),
                prefix: String::new(),
                suffix: String::new(),
                priority,
                visibility: SuiteVisibility::default(),
                tool_aliases: HashMap::new(),
                hidden_tools: HashSet::new(),
                tools,
                prefix_char: None,
            },
        );
        self.flush_cache();
        Ok(())
    }

    /// Remove a context from the suite.
    pub fn remove_context(&mut self, name: &str) -> Result<()> {
        self.require_context(name)?;
        self.contexts.remove(name);
        self.flush_cache();
        Ok(())
    }

    /// Check if a context exists in the suite.
    pub fn has_context(&self, name: &str) -> bool {
        self.contexts.contains_key(name)
    }

    /// Get sorted context names.
    pub fn context_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.contexts.keys().map(|s| s.as_str()).collect();
        names.sort();
        names
    }

    /// Get context info (read-only).
    pub fn context(&self, name: &str) -> Result<&SuiteContext> {
        self.contexts
            .get(name)
            .ok_or_else(|| RezError::Suite(format!("No such context: {name:?}")))
    }

    /// Get path to the .rxt file for a context within this suite.
    pub fn context_path(&self, name: &str) -> Option<PathBuf> {
        self.load_path
            .as_ref()
            .map(|lp| lp.join("contexts").join(format!("{name}.rxt")))
    }

    // -- Prefix / Suffix --

    /// Set a prefix applied to all tool names in a context.
    pub fn set_context_prefix(&mut self, name: &str, prefix: &str) -> Result<()> {
        let ctx = self.require_context_mut(name)?;
        ctx.prefix = prefix.to_string();
        self.flush_cache();
        Ok(())
    }

    /// Remove a context's prefix.
    pub fn remove_context_prefix(&mut self, name: &str) -> Result<()> {
        self.set_context_prefix(name, "")
    }

    /// Set a suffix applied to all tool names in a context.
    pub fn set_context_suffix(&mut self, name: &str, suffix: &str) -> Result<()> {
        let ctx = self.require_context_mut(name)?;
        ctx.suffix = suffix.to_string();
        self.flush_cache();
        Ok(())
    }

    /// Remove a context's suffix.
    pub fn remove_context_suffix(&mut self, name: &str) -> Result<()> {
        self.set_context_suffix(name, "")
    }

    // -- Priority --

    /// Bump a context so its tools take priority over all others.
    pub fn bump_context(&mut self, name: &str) -> Result<()> {
        let p = self.next_priority();
        let ctx = self.require_context_mut(name)?;
        ctx.priority = p;
        self.flush_cache();
        Ok(())
    }

    // -- Tool hiding --

    /// Hide a tool so it is not exposed in the suite.
    pub fn hide_tool(&mut self, context_name: &str, tool_name: &str) -> Result<()> {
        self.validate_tool(context_name, tool_name)?;
        let ctx = self.require_context_mut(context_name)?;
        ctx.hidden_tools.insert(tool_name.to_string());
        self.flush_cache();
        Ok(())
    }

    /// Unhide a previously hidden tool.
    pub fn unhide_tool(&mut self, context_name: &str, tool_name: &str) -> Result<()> {
        let ctx = self.require_context_mut(context_name)?;
        ctx.hidden_tools.remove(tool_name);
        self.flush_cache();
        Ok(())
    }

    // -- Tool aliasing --

    /// Register an alias for a specific tool. Aliases override prefix/suffix.
    pub fn alias_tool(
        &mut self,
        context_name: &str,
        tool_name: &str,
        tool_alias: &str,
    ) -> Result<()> {
        self.validate_tool(context_name, tool_name)?;
        let ctx = self.require_context_mut(context_name)?;
        if ctx.tool_aliases.contains_key(tool_name) {
            let existing = &ctx.tool_aliases[tool_name];
            return Err(RezError::Suite(format!(
                "Tool {tool_name:?} in context {context_name:?} is already aliased to {existing:?}"
            )));
        }
        ctx.tool_aliases
            .insert(tool_name.to_string(), tool_alias.to_string());
        self.flush_cache();
        Ok(())
    }

    /// Remove a tool's alias.
    pub fn unalias_tool(&mut self, context_name: &str, tool_name: &str) -> Result<()> {
        let ctx = self.require_context_mut(context_name)?;
        if ctx.tool_aliases.remove(tool_name).is_some() {
            self.flush_cache();
        }
        Ok(())
    }

    // -- Tool queries --

    /// Get visible tools exposed by this suite, keyed by alias.
    pub fn get_tools(&mut self) -> &HashMap<String, SuiteTool> {
        self.update_cache();
        &self.cache.as_ref().expect("cache populated").tools
    }

    /// Get tools that have been explicitly hidden.
    pub fn get_hidden_tools(&mut self) -> &[SuiteTool] {
        self.update_cache();
        &self.cache.as_ref().expect("cache populated").hidden
    }

    /// Get aliases that have conflicts (same alias from different contexts).
    pub fn get_conflicting_aliases(&mut self) -> Vec<String> {
        self.update_cache();
        self.cache
            .as_ref()
            .expect("cache populated")
            .conflicts
            .keys()
            .cloned()
            .collect()
    }

    /// Get the list of conflicting entries for a given alias.
    pub fn get_alias_conflicts(&mut self, tool_alias: &str) -> Option<&Vec<SuiteTool>> {
        self.update_cache();
        self.cache
            .as_ref()
            .expect("cache populated")
            .conflicts
            .get(tool_alias)
    }

    /// Get the filesystem path to a tool wrapper script.
    pub fn get_tool_filepath(&mut self, tool_alias: &str) -> Option<PathBuf> {
        let has_tool = self.get_tools().contains_key(tool_alias);
        if has_tool {
            self.tools_path().map(|tp| tp.join(tool_alias))
        } else {
            None
        }
    }

    /// Get the context name that provides a given tool alias.
    pub fn get_tool_context(&mut self, tool_alias: &str) -> Option<String> {
        self.update_cache();
        self.cache
            .as_ref()
            .and_then(|c| c.tools.get(tool_alias))
            .map(|t| t.context_name.clone())
    }

    /// Get the bin directory path (only valid if suite was loaded from disk).
    pub fn tools_path(&self) -> Option<PathBuf> {
        self.load_path.as_ref().map(|lp| lp.join("bin"))
    }

    // -- Display --

    /// Print suite summary info.
    pub fn print_info(&mut self, verbose: bool) {
        if self.contexts.is_empty() {
            println!("Suite is empty.");
            return;
        }
        let names = self.context_names();
        println!("Suite contains {} contexts:", names.len());

        if !verbose {
            println!("{}", names.join(" "));
            return;
        }

        // Collect tool counts per context
        let mut ctx_tool_count: HashMap<String, usize> = HashMap::new();
        for tool in self.get_tools().values() {
            *ctx_tool_count.entry(tool.context_name.clone()).or_default() += 1;
        }

        println!();
        println!("{:<20} {:<20} PATH", "NAME", "VISIBLE TOOLS");
        println!("{:<20} {:<20} ----", "----", "-------------");

        let names = self.context_names();
        for name in &names {
            let ntools = ctx_tool_count.get(*name).copied().unwrap_or(0);
            let desc = if ntools > 0 {
                format!("{ntools} tools")
            } else {
                "no tools".to_string()
            };
            let path = self
                .context_path(name)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "-".to_string());
            println!("{:<20} {:<20} {}", name, desc, path);
        }
    }

    /// Print table of tools available in the suite.
    pub fn print_tools(&mut self, verbose: bool, filter_context: Option<&str>) {
        if let Some(cn) = filter_context {
            if !self.has_context(cn) {
                println!("No such context: {cn:?}");
                return;
            }
        }

        self.update_cache();
        let cache = self.cache.as_ref().expect("cache populated");

        println!("{:<20} {:<20} {:<20} ", "TOOL", "ALIASING", "CONTEXT");
        println!("{:<20} {:<20} {:<20} ", "----", "--------", "-------");

        let mut entries: Vec<&SuiteTool> = cache.tools.values().collect();
        if let Some(cn) = filter_context {
            entries.retain(|t| t.context_name == cn);
        }
        entries.sort_by_key(|entry| entry.tool_alias.to_lowercase());

        for entry in &entries {
            let aliasing = if entry.tool_name == entry.tool_alias {
                "-".to_string()
            } else {
                entry.tool_name.clone()
            };
            let props = if entry.tool_name != entry.tool_alias {
                "(aliased)"
            } else {
                ""
            };
            println!(
                "{:<20} {:<20} {:<20} {}",
                entry.tool_alias, aliasing, entry.context_name, props
            );
        }

        if verbose {
            // Show hidden tools
            for entry in &cache.hidden {
                if filter_context.is_some_and(|cn| cn != entry.context_name) {
                    continue;
                }
                let aliasing = if entry.tool_name == entry.tool_alias {
                    "-".to_string()
                } else {
                    entry.tool_name.clone()
                };
                println!(
                    "{:<20} {:<20} {:<20} (hidden)",
                    entry.tool_alias, aliasing, entry.context_name
                );
            }

            // Show conflicting tools
            for entries in cache.conflicts.values() {
                for entry in entries {
                    if filter_context.is_some_and(|cn| cn != entry.context_name) {
                        continue;
                    }
                    let aliasing = if entry.tool_name == entry.tool_alias {
                        "-".to_string()
                    } else {
                        entry.tool_name.clone()
                    };
                    println!(
                        "{:<20} {:<20} {:<20} (not visible)",
                        entry.tool_alias, aliasing, entry.context_name
                    );
                }
            }
        }

        if entries.is_empty() && cache.hidden.is_empty() {
            println!("No tools available.");
        }
    }

    /// Find visible suite directories on PATH.
    pub fn visible_suite_paths(paths: Option<&[&str]>) -> Vec<PathBuf> {
        let path_str;
        let search_paths: Vec<&str> = if let Some(p) = paths {
            p.to_vec()
        } else {
            path_str = std::env::var("PATH").unwrap_or_default();
            path_str
                .split(if cfg!(windows) { ';' } else { ':' })
                .collect()
        };

        let mut suite_paths = Vec::new();
        for p in search_paths {
            if p.is_empty() {
                continue;
            }
            let dir = Path::new(p);
            if dir.is_dir() {
                // bin/ dir -> check parent for suite.yaml or suite.json
                if let Some(parent) = dir.parent() {
                    if parent.join("suite.yaml").is_file() || parent.join("suite.json").is_file() {
                        suite_paths.push(parent.to_path_buf());
                    }
                }
            }
        }
        suite_paths
    }

    // -- Wrapper generation --

    /// Generate executable wrapper scripts for all visible tools.
    ///
    /// Matches Python rez format: wrapper file contains YAML (after batch header on Windows).
    /// When executed, invokes `rez forward <wrapper_path> [args]` so forward reads YAML from the file.
    fn generate_wrappers(&self, bin_dir: &Path, _suite_path: &Path) -> Result<()> {
        use crate::config::CONFIG;

        let tools = self.compute_tools();
        let default_prefix = format!("{}", CONFIG.suite_alias_prefix_char);

        for (alias, tool) in &tools.tools {
            let ctx = self.contexts.get(&tool.context_name);
            let prefix_char = ctx
                .and_then(|c| c.prefix_char.as_deref())
                .unwrap_or(&default_prefix);

            // YAML body: Python rez format (module/suite, kwargs for context_name, tool_name, prefix_char)
            let yaml_body = serde_yaml::to_string(&serde_json::json!({
                "module": "suite",
                "func_name": "_FWD__invoke_suite_tool_alias",
                "kwargs": {
                    "context_name": tool.context_name,
                    "tool_name": tool.tool_name,
                    "prefix_char": prefix_char,
                },
            }))
            .map_err(|e| RezError::Suite(format!("Failed serializing YAML: {e}")))?;

            if cfg!(windows) {
                let wrapper_path = bin_dir.join(format!("{alias}.cmd"));
                let content = format!(
                    "@echo off\r\n\
                    rez forward \"%~dpnx0\" -- %*\r\n\
                    goto :eof\r\n\
                    :: YAML\r\n\
                    {yaml}",
                    yaml = yaml_body,
                );
                std::fs::write(&wrapper_path, content)?;
            } else {
                let wrapper_path = bin_dir.join(alias);
                let content = format!("#!/usr/bin/env -S rez forward\n{yaml}", yaml = yaml_body);
                std::fs::write(&wrapper_path, content)?;

                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perms = std::fs::metadata(&wrapper_path)?.permissions();
                    perms.set_mode(0o755);
                    std::fs::set_permissions(&wrapper_path, perms)?;
                }
            }
        }
        Ok(())
    }

    // -- Internal helpers --

    /// Get next auto-incrementing priority value.
    fn next_priority(&mut self) -> i32 {
        let p = self.next_priority;
        self.next_priority += 1;
        p
    }

    /// Require a context exists, return error if not.
    fn require_context(&self, name: &str) -> Result<&SuiteContext> {
        self.contexts
            .get(name)
            .ok_or_else(|| RezError::Suite(format!("No such context: {name:?}")))
    }

    /// Require a context exists (mutable), return error if not.
    fn require_context_mut(&mut self, name: &str) -> Result<&mut SuiteContext> {
        self.contexts
            .get_mut(name)
            .ok_or_else(|| RezError::Suite(format!("No such context: {name:?}")))
    }

    /// Validate that a tool exists in a context.
    fn validate_tool(&self, context_name: &str, tool_name: &str) -> Result<()> {
        let ctx = self.require_context(context_name)?;
        if !ctx.tools.contains(&tool_name.to_string()) {
            return Err(RezError::Suite(format!(
                "No such tool {tool_name:?} in context {context_name:?}"
            )));
        }
        Ok(())
    }

    /// Invalidate cached tool resolution.
    fn flush_cache(&mut self) {
        self.cache = None;
    }

    /// Recompute tool cache if needed.
    fn update_cache(&mut self) {
        if self.cache.is_some() {
            return;
        }
        self.cache = Some(self.compute_tools());
    }

    /// Compute resolved tools from all contexts. Pure function, no mutation.
    fn compute_tools(&self) -> ToolCache {
        let mut result = ToolCache::default();

        // Sort contexts by priority, iterate highest-priority first
        let mut sorted: Vec<&SuiteContext> = self.contexts.values().collect();
        sorted.sort_by_key(|c| c.priority);

        for ctx in sorted.iter().rev() {
            for tool_name in &ctx.tools {
                // Compute alias: explicit alias overrides prefix/suffix
                let alias = if let Some(a) = ctx.tool_aliases.get(tool_name) {
                    a.clone()
                } else {
                    format!("{}{}{}", ctx.prefix, tool_name, ctx.suffix)
                };

                let entry = SuiteTool {
                    tool_name: tool_name.clone(),
                    tool_alias: alias.clone(),
                    context_name: ctx.name.clone(),
                    package: None,
                };

                // Skip hidden tools
                if ctx.hidden_tools.contains(tool_name) {
                    result.hidden.push(entry);
                    continue;
                }

                // Handle conflicts
                if let Some(existing) = result.tools.get(&alias) {
                    if existing.context_name != ctx.name {
                        // Different context -> conflict (existing wins by priority)
                        result.conflicts.entry(alias).or_default().push(entry);
                    }
                    // Same context, same alias -> first one wins (higher priority)
                } else {
                    result.tools.insert(alias, entry);
                }
            }
        }

        result
    }
}

impl Default for Suite {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for Suite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = self.context_names();
        write!(f, "Suite({})", names.join(" "))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_suite() -> Suite {
        let mut suite = Suite::new();
        suite
            .add_context(
                "maya",
                Path::new("/tmp/maya.rxt"),
                vec!["render".into(), "maya".into(), "mayapy".into()],
            )
            .unwrap();
        suite
            .add_context(
                "houdini",
                Path::new("/tmp/houdini.rxt"),
                vec!["hython".into(), "render".into(), "houdini".into()],
            )
            .unwrap();
        suite
    }

    #[test]
    fn test_new_suite() {
        let suite = Suite::new();
        assert!(suite.context_names().is_empty());
        assert!(suite.load_path.is_none());
    }

    #[test]
    fn test_add_context() {
        let mut suite = Suite::new();
        suite
            .add_context("test", Path::new("/tmp/test.rxt"), vec!["tool1".into()])
            .unwrap();
        assert!(suite.has_context("test"));
        assert_eq!(suite.context_names(), vec!["test"]);
    }

    #[test]
    fn test_add_duplicate_context() {
        let mut suite = Suite::new();
        suite
            .add_context("test", Path::new("/tmp/test.rxt"), vec!["tool1".into()])
            .unwrap();
        let err = suite
            .add_context("test", Path::new("/tmp/test2.rxt"), vec!["tool2".into()])
            .unwrap_err();
        assert!(err.to_string().contains("already in suite"));
    }

    #[test]
    fn test_remove_context() {
        let mut suite = make_suite();
        assert!(suite.has_context("maya"));
        suite.remove_context("maya").unwrap();
        assert!(!suite.has_context("maya"));
    }

    #[test]
    fn test_remove_nonexistent_context() {
        let mut suite = Suite::new();
        let err = suite.remove_context("nope").unwrap_err();
        assert!(err.to_string().contains("No such context"));
    }

    #[test]
    fn test_context_names_sorted() {
        let suite = make_suite();
        assert_eq!(suite.context_names(), vec!["houdini", "maya"]);
    }

    #[test]
    fn test_tool_resolution_basic() {
        let mut suite = Suite::new();
        suite
            .add_context(
                "ctx",
                Path::new("/tmp/ctx.rxt"),
                vec!["foo".into(), "bar".into()],
            )
            .unwrap();
        let tools = suite.get_tools();
        assert_eq!(tools.len(), 2);
        assert!(tools.contains_key("foo"));
        assert!(tools.contains_key("bar"));
    }

    #[test]
    fn test_tool_conflict_resolution() {
        // "houdini" added second -> higher priority -> "render" belongs to houdini
        let mut suite = make_suite();
        let tools = suite.get_tools();
        let render = tools.get("render").unwrap();
        assert_eq!(render.context_name, "houdini");

        // "render" should also be in conflicts from maya
        let conflicts = suite.get_conflicting_aliases();
        assert!(conflicts.contains(&"render".to_string()));
    }

    #[test]
    fn test_bump_context_priority() {
        let mut suite = make_suite();
        // Bump maya -> now maya's render wins
        suite.bump_context("maya").unwrap();
        let tools = suite.get_tools();
        let render = tools.get("render").unwrap();
        assert_eq!(render.context_name, "maya");
    }

    #[test]
    fn test_prefix_suffix() {
        let mut suite = Suite::new();
        suite
            .add_context("maya", Path::new("/tmp/maya.rxt"), vec!["render".into()])
            .unwrap();
        suite.set_context_prefix("maya", "maya_").unwrap();
        let tools = suite.get_tools();
        assert!(tools.contains_key("maya_render"));
        assert!(!tools.contains_key("render"));

        // Now add suffix too
        suite.set_context_suffix("maya", "_2024").unwrap();
        let tools = suite.get_tools();
        assert!(tools.contains_key("maya_render_2024"));
    }

    #[test]
    fn test_remove_prefix() {
        let mut suite = Suite::new();
        suite
            .add_context("maya", Path::new("/tmp/maya.rxt"), vec!["render".into()])
            .unwrap();
        suite.set_context_prefix("maya", "maya_").unwrap();
        suite.remove_context_prefix("maya").unwrap();
        let tools = suite.get_tools();
        assert!(tools.contains_key("render"));
    }

    #[test]
    fn test_hide_tool() {
        let mut suite = Suite::new();
        suite
            .add_context(
                "ctx",
                Path::new("/tmp/ctx.rxt"),
                vec!["foo".into(), "bar".into()],
            )
            .unwrap();
        suite.hide_tool("ctx", "foo").unwrap();
        let tools = suite.get_tools();
        assert!(!tools.contains_key("foo"));
        assert!(tools.contains_key("bar"));

        let hidden = suite.get_hidden_tools();
        assert_eq!(hidden.len(), 1);
        assert_eq!(hidden[0].tool_name, "foo");
    }

    #[test]
    fn test_unhide_tool() {
        let mut suite = Suite::new();
        suite
            .add_context("ctx", Path::new("/tmp/ctx.rxt"), vec!["foo".into()])
            .unwrap();
        suite.hide_tool("ctx", "foo").unwrap();
        assert!(suite.get_tools().is_empty());

        suite.unhide_tool("ctx", "foo").unwrap();
        assert!(suite.get_tools().contains_key("foo"));
    }

    #[test]
    fn test_hide_nonexistent_tool() {
        let mut suite = Suite::new();
        suite
            .add_context("ctx", Path::new("/tmp/ctx.rxt"), vec!["foo".into()])
            .unwrap();
        let err = suite.hide_tool("ctx", "nope").unwrap_err();
        assert!(err.to_string().contains("No such tool"));
    }

    #[test]
    fn test_alias_tool() {
        let mut suite = Suite::new();
        suite
            .add_context("ctx", Path::new("/tmp/ctx.rxt"), vec!["foo".into()])
            .unwrap();
        suite.alias_tool("ctx", "foo", "my_foo").unwrap();
        let tools = suite.get_tools();
        assert!(tools.contains_key("my_foo"));
        assert!(!tools.contains_key("foo"));
        assert_eq!(tools["my_foo"].tool_name, "foo");
    }

    #[test]
    fn test_alias_overrides_prefix() {
        let mut suite = Suite::new();
        suite
            .add_context(
                "ctx",
                Path::new("/tmp/ctx.rxt"),
                vec!["foo".into(), "bar".into()],
            )
            .unwrap();
        suite.set_context_prefix("ctx", "pfx_").unwrap();
        suite.alias_tool("ctx", "foo", "my_foo").unwrap();

        let tools = suite.get_tools();
        // foo should be aliased, not prefixed
        assert!(tools.contains_key("my_foo"));
        assert!(!tools.contains_key("pfx_foo"));
        // bar should be prefixed normally
        assert!(tools.contains_key("pfx_bar"));
    }

    #[test]
    fn test_alias_duplicate_error() {
        let mut suite = Suite::new();
        suite
            .add_context("ctx", Path::new("/tmp/ctx.rxt"), vec!["foo".into()])
            .unwrap();
        suite.alias_tool("ctx", "foo", "alias1").unwrap();
        let err = suite.alias_tool("ctx", "foo", "alias2").unwrap_err();
        assert!(err.to_string().contains("already aliased"));
    }

    #[test]
    fn test_unalias_tool() {
        let mut suite = Suite::new();
        suite
            .add_context("ctx", Path::new("/tmp/ctx.rxt"), vec!["foo".into()])
            .unwrap();
        suite.alias_tool("ctx", "foo", "my_foo").unwrap();
        suite.unalias_tool("ctx", "foo").unwrap();
        let tools = suite.get_tools();
        assert!(tools.contains_key("foo"));
        assert!(!tools.contains_key("my_foo"));
    }

    #[test]
    fn test_get_tool_context() {
        let mut suite = make_suite();
        let ctx = suite.get_tool_context("maya").unwrap();
        assert_eq!(ctx, "maya");

        assert!(suite.get_tool_context("nonexistent").is_none());
    }

    #[test]
    fn test_display() {
        let suite = make_suite();
        let s = suite.to_string();
        assert!(s.contains("Suite("));
        assert!(s.contains("houdini"));
        assert!(s.contains("maya"));
    }

    #[test]
    fn test_save_and_load() {
        let dir = std::env::temp_dir().join("rez_suite_test_save_load");
        if dir.exists() {
            fs::remove_dir_all(&dir).unwrap();
        }

        let mut suite = Suite::new();
        // Create a dummy .rxt file
        let rxt_dir = std::env::temp_dir().join("rez_suite_test_rxt");
        fs::create_dir_all(&rxt_dir).unwrap();
        let rxt_path = rxt_dir.join("myctx.rxt");
        fs::write(&rxt_path, "{}").unwrap();

        suite
            .add_context("myctx", &rxt_path, vec!["tool_a".into(), "tool_b".into()])
            .unwrap();
        suite.set_context_prefix("myctx", "my_").unwrap();
        suite.hide_tool("myctx", "tool_b").unwrap();

        suite.save(&dir).unwrap();

        // Verify suite.yaml exists (new format)
        assert!(dir.join("suite.yaml").is_file());
        assert!(dir.join("contexts").join("myctx.rxt").is_file());
        assert!(dir.join("bin").is_dir());

        // Load back
        let mut loaded = Suite::load(&dir).unwrap();
        assert!(loaded.has_context("myctx"));
        assert_eq!(loaded.context_names(), vec!["myctx"]);

        let tools = loaded.get_tools();
        assert!(tools.contains_key("my_tool_a"));
        assert!(!tools.contains_key("my_tool_b")); // hidden
        assert!(!tools.contains_key("tool_a")); // prefixed

        // Cleanup
        fs::remove_dir_all(&dir).unwrap();
        fs::remove_dir_all(&rxt_dir).unwrap();
    }

    #[test]
    fn test_load_nonexistent() {
        let result = Suite::load(Path::new("/nonexistent/path"));
        assert!(result.is_err());
    }

    #[test]
    fn test_save_path_exists_error() {
        let dir = std::env::temp_dir().join("rez_suite_test_exists");
        fs::create_dir_all(&dir).unwrap();

        let suite = Suite::new();
        let err = suite.save(&dir).unwrap_err();
        assert!(err.to_string().contains("path exists"));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_visible_suite_paths_empty() {
        let paths = Suite::visible_suite_paths(Some(&["/nonexistent/bin"]));
        assert!(paths.is_empty());
    }

    #[test]
    fn test_context_info() {
        let suite = make_suite();
        let ctx = suite.context("maya").unwrap();
        assert_eq!(ctx.name, "maya");
        assert_eq!(ctx.tools.len(), 3);
    }

    #[test]
    fn test_complex_scenario() {
        // Multiple contexts, prefixes, aliases, hidden tools
        let mut suite = Suite::new();
        suite
            .add_context(
                "maya",
                Path::new("/tmp/m.rxt"),
                vec!["render".into(), "maya".into(), "mayapy".into()],
            )
            .unwrap();
        suite
            .add_context(
                "houdini",
                Path::new("/tmp/h.rxt"),
                vec!["render".into(), "houdini".into(), "hython".into()],
            )
            .unwrap();

        // Prefix houdini to avoid render conflict
        suite.set_context_prefix("houdini", "hou_").unwrap();
        // Alias maya render
        suite.alias_tool("maya", "render", "maya_render").unwrap();
        // Hide mayapy
        suite.hide_tool("maya", "mayapy").unwrap();

        let tools = suite.get_tools();
        assert!(tools.contains_key("maya_render")); // aliased
        assert!(tools.contains_key("maya")); // original name, no prefix
        assert!(tools.contains_key("hou_render")); // prefixed
        assert!(tools.contains_key("hou_houdini")); // prefixed
        assert!(tools.contains_key("hou_hython")); // prefixed
        assert!(!tools.contains_key("mayapy")); // hidden
        assert!(!tools.contains_key("render")); // aliased away

        // No conflicts since we resolved them
        let conflicts = suite.get_conflicting_aliases();
        assert!(conflicts.is_empty());

        // One hidden tool
        let hidden = suite.get_hidden_tools();
        assert_eq!(hidden.len(), 1);
        assert_eq!(hidden[0].tool_name, "mayapy");
    }

    #[test]
    fn test_default_trait() {
        let suite = Suite::default();
        assert!(suite.context_names().is_empty());
    }

    #[test]
    fn test_tools_path() {
        let suite = Suite::new();
        assert!(suite.tools_path().is_none());
    }

    #[test]
    fn test_context_path() {
        let suite = Suite::new();
        assert!(suite.context_path("foo").is_none());
    }
}
