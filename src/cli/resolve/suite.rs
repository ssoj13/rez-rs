//! `rez suite` - manage suites of contexts with unified tools.

use std::path::{Path, PathBuf};

use clap::Args;

use foundation::errors::{Result, RezError};
use resolve::suite::Suite;

/// Manage a suite or print information about an existing suite.
#[derive(Args, Debug)]
pub struct SuiteArgs {
    /// Directory of suite to create or manage
    #[arg(value_name = "DIR")]
    pub dir: Option<PathBuf>,

    /// List visible suites on PATH
    #[arg(short, long)]
    pub list: bool,

    /// Print a list of tools available in the suite
    #[arg(short = 't', long = "tools")]
    pub print_tools: bool,

    /// Print path to a tool in the suite, if it exists
    #[arg(long, value_name = "TOOL")]
    pub which: Option<String>,

    /// Validate the suite
    #[arg(long)]
    pub validate: bool,

    /// Create an empty suite at DIR
    #[arg(long)]
    pub create: bool,

    /// Context name (required for context-specific options like --add)
    #[arg(short = 'c', long, value_name = "NAME")]
    pub context: Option<String>,

    /// Add a context (.rxt file) to the suite (requires --context)
    #[arg(short = 'a', long, value_name = "RXT")]
    pub add: Option<PathBuf>,

    /// Prefix char for rez options via suite tool (with --add, default: '+')
    #[arg(short = 'P', long, value_name = "CHAR")]
    pub prefix_char: Option<String>,

    /// Remove a context from the suite
    #[arg(short = 'r', long, value_name = "NAME")]
    pub remove: Option<String>,

    /// Set the prefix of a context in the suite (requires --context)
    #[arg(short = 'p', long, value_name = "PREFIX")]
    pub prefix: Option<String>,

    /// Set the suffix of a context in the suite (requires --context)
    #[arg(short = 's', long, value_name = "SUFFIX")]
    pub suffix: Option<String>,

    /// Hide a tool of a context in the suite (requires --context)
    #[arg(long, value_name = "TOOL")]
    pub hide: Option<String>,

    /// Unhide a tool of a context in the suite (requires --context)
    #[arg(long, value_name = "TOOL")]
    pub unhide: Option<String>,

    /// Create an alias for a tool (TOOL ALIAS, requires --context)
    #[arg(long, num_args = 2, value_names = ["TOOL", "ALIAS"])]
    pub alias: Option<Vec<String>>,

    /// Remove an alias for a tool in the suite (requires --context)
    #[arg(long, value_name = "TOOL")]
    pub unalias: Option<String>,

    /// Bump a context, making its tools higher priority
    #[arg(short = 'b', long, value_name = "NAME")]
    pub bump: Option<String>,

    /// Verbose output
    #[arg(from_global)]
    pub verbose: u8,
}

impl SuiteArgs {
    /// Require --context and return its value, or error.
    fn require_context(&self, opt_name: &str) -> Result<&str> {
        self.context.as_deref().ok_or_else(|| {
            RezError::Suite(format!(
                "--context must be supplied when using --{}",
                opt_name
            ))
        })
    }

    /// Whether any save-triggering option is set.
    fn needs_save(&self) -> bool {
        self.add.is_some()
            || self.remove.is_some()
            || self.bump.is_some()
            || self.prefix.is_some()
            || self.suffix.is_some()
            || self.hide.is_some()
            || self.unhide.is_some()
            || self.alias.is_some()
            || self.unalias.is_some()
    }
}

pub fn run(args: &SuiteArgs) -> Result<()> {
    // --list: show visible suites and exit
    if args.list {
        let suites = Suite::visible_suite_paths(None);
        if suites.is_empty() {
            println!("No visible suites.");
        } else {
            for s in &suites {
                println!("{}", s.display());
            }
        }
        return Ok(());
    }

    // Everything else requires DIR
    let dir = match &args.dir {
        Some(d) => d.clone(),
        None => {
            return Err(RezError::Suite("DIR argument is required".into()));
        }
    };

    // --create: create empty suite
    if args.create {
        if args.verbose > 0 {
            println!("Creating empty suite at {:?}...", dir);
        }
        let suite = Suite::new();
        suite.save(&dir)?;
        return Ok(());
    }

    // Load existing suite
    let mut suite = Suite::load(&dir)?;

    // --validate
    if args.validate {
        // If suite loaded successfully, it's structurally valid
        println!("The suite is valid.");
        return Ok(());
    }

    // --which TOOL
    if let Some(ref tool) = args.which {
        if let Some(path) = suite.get_tool_filepath(tool) {
            println!("{}", path.display());
        } else {
            return Err(RezError::Suite(format!(
                "tool '{}' not found in suite",
                tool
            )));
        }
        return Ok(());
    }

    // --tools: print tool listing
    if args.print_tools {
        suite.print_tools(args.verbose > 0, args.context.as_deref());
        return Ok(());
    }

    // --add RXT (requires --context)
    if let Some(ref rxt_path) = args.add {
        let ctx_name = args.require_context("add")?;
        if args.verbose > 0 {
            println!("Loading context at {:?}...", rxt_path);
            println!("Adding context {:?}...", ctx_name);
        }
        // Read tools from the context before adding it to the suite.
        let tools = read_rxt_tools(rxt_path)?;
        suite.add_context(ctx_name, rxt_path, tools)?;
    }
    // --remove NAME
    else if let Some(ref name) = args.remove {
        if args.verbose > 0 {
            println!("Removing context {:?}...", name);
        }
        suite.remove_context(name)?;
    }
    // --bump NAME
    else if let Some(ref name) = args.bump {
        if args.verbose > 0 {
            println!("Bumping context {:?}...", name);
        }
        suite.bump_context(name)?;
    }
    // --prefix (requires --context)
    else if let Some(ref prefix) = args.prefix {
        let ctx_name = args.require_context("prefix")?;
        if args.verbose > 0 {
            println!("Setting prefix on context {:?}...", ctx_name);
        }
        suite.set_context_prefix(ctx_name, prefix)?;
    }
    // --suffix (requires --context)
    else if let Some(ref suffix) = args.suffix {
        let ctx_name = args.require_context("suffix")?;
        if args.verbose > 0 {
            println!("Setting suffix on context {:?}...", ctx_name);
        }
        suite.set_context_suffix(ctx_name, suffix)?;
    }
    // --hide TOOL (requires --context)
    else if let Some(ref tool) = args.hide {
        let ctx_name = args.require_context("hide")?;
        if args.verbose > 0 {
            println!("Hiding tool {:?} in context {:?}...", tool, ctx_name);
        }
        suite.hide_tool(ctx_name, tool)?;
    }
    // --unhide TOOL (requires --context)
    else if let Some(ref tool) = args.unhide {
        let ctx_name = args.require_context("unhide")?;
        if args.verbose > 0 {
            println!("Unhiding tool {:?} in context {:?}...", tool, ctx_name);
        }
        suite.unhide_tool(ctx_name, tool)?;
    }
    // --alias TOOL ALIAS (requires --context)
    else if let Some(ref alias_args) = args.alias {
        let ctx_name = args.require_context("alias")?;
        let tool = &alias_args[0];
        let alias = &alias_args[1];
        if args.verbose > 0 {
            println!(
                "Aliasing tool {:?} as {:?} in context {:?}...",
                tool, alias, ctx_name
            );
        }
        suite.alias_tool(ctx_name, tool, alias)?;
    }
    // --unalias TOOL (requires --context)
    else if let Some(ref tool) = args.unalias {
        let ctx_name = args.require_context("unalias")?;
        if args.verbose > 0 {
            println!("Unaliasing tool {:?} in context {:?}...", tool, ctx_name);
        }
        suite.unalias_tool(ctx_name, tool)?;
    }
    // --context NAME (without action): show context info
    else if let Some(ref ctx_name) = args.context {
        let ctx = suite.context(ctx_name)?;
        println!("Context: {}", ctx.name);
        println!("  Path: {}", ctx.context_path.display());
        println!("  Priority: {}", ctx.priority);
        println!("  Prefix: {:?}", ctx.prefix);
        println!("  Suffix: {:?}", ctx.suffix);
        if !ctx.tools.is_empty() {
            println!("  Tools: {}", ctx.tools.join(", "));
        }
        if !ctx.hidden_tools.is_empty() {
            let mut hidden: Vec<&String> = ctx.hidden_tools.iter().collect();
            hidden.sort();
            println!(
                "  Hidden: {}",
                hidden
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if !ctx.tool_aliases.is_empty() {
            for (from, to) in &ctx.tool_aliases {
                println!("  Alias: {} -> {}", from, to);
            }
        }
        return Ok(());
    }
    // Default: print suite info
    else {
        suite.print_info(args.verbose > 0);
        return Ok(());
    }

    // Save if a modifying operation was performed
    if args.needs_save() {
        if args.verbose > 0 {
            println!("Saving suite to {:?}...", dir);
        }
        suite.save(&dir)?;
    }

    Ok(())
}

/// Read tool names from an .rxt file.
/// Uses "tools" array if present; otherwise loads context and derives from resolved packages (bin/ scan).
fn read_rxt_tools(rxt_path: &Path) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(rxt_path).map_err(|error| {
        RezError::Suite(format!("failed to read {}: {error}", rxt_path.display()))
    })?;
    let data = serde_yaml::from_str::<serde_json::Value>(&content).map_err(|error| {
        RezError::Suite(format!("failed to parse {}: {error}", rxt_path.display()))
    })?;

    if let Some(tools) = data.get("tools").and_then(|value| value.as_array()) {
        return Ok(tools
            .iter()
            .filter_map(|value| value.as_str().map(String::from))
            .collect());
    }

    // Fallback: load context and derive tools from resolved packages (scans bin/ dirs)
    let context = resolve::context::ResolvedContext::load(rxt_path, None)?;
    let mut all_tools: Vec<String> = context
        .get_tools(true)?
        .into_values()
        .flat_map(|(_, tools)| tools)
        .collect();
    all_tools.sort();
    all_tools.dedup();
    Ok(all_tools)
}

#[cfg(test)]
mod tests {
    use super::*;
    use repository::FilesystemPackageProvider;
    use resolve::context::{ResolveOptions, ResolvedContext};
    use version::Requirement;

    fn write_package(repo: &Path, name: &str, definition: &str) {
        let package_dir = repo.join(name).join("1.0.0");
        std::fs::create_dir_all(&package_dir).expect("create package directory");
        std::fs::write(package_dir.join("package.yaml"), definition)
            .expect("write package definition");
    }

    #[test]
    fn read_rxt_tools_supports_yaml_and_only_exposes_requested_packages() {
        let repo = tempfile::tempdir().expect("repository tempdir");
        write_package(
            repo.path(),
            "root",
            "name: root\nversion: 1.0.0\nrequires:\n  - dep\ntools:\n  - root_tool\n",
        );
        write_package(
            repo.path(),
            "dep",
            "name: dep\nversion: 1.0.0\ntools:\n  - dependency_tool\n",
        );

        let provider = FilesystemPackageProvider::from_path(repo.path()).unwrap();
        let context = ResolvedContext::resolve(
            vec![Requirement::new("root").unwrap()],
            &provider,
            ResolveOptions {
                package_paths: Some(vec![repo.path().to_path_buf()]),
                add_implicit: false,
                caching: false,
                ..ResolveOptions::default()
            },
        )
        .expect("resolve root and dependency");
        let yaml = serde_yaml::to_string(&context.to_json().expect("serialize context"))
            .expect("serialize YAML context");
        let yaml_path = repo.path().join("context.yaml.rxt");
        std::fs::write(&yaml_path, yaml).expect("write YAML context");

        assert_eq!(
            read_rxt_tools(&yaml_path).expect("read YAML context tools"),
            vec!["root_tool"]
        );

        let legacy_path = repo.path().join("legacy.rxt");
        std::fs::write(&legacy_path, r#"{"tools":["legacy_tool"]}"#)
            .expect("write legacy tools metadata");
        assert_eq!(
            read_rxt_tools(&legacy_path).expect("read legacy tools metadata"),
            vec!["legacy_tool"]
        );
    }
}
