// SPDX-License-Identifier: Apache-2.0

//! Rez-rs CLI binary - Rust implementation of the Rez package manager.
//!
//! Single binary providing all rez commands as subcommands.
//! Help output is grouped by category with a studio setup walkthrough.

use std::path::PathBuf;

use clap::{ArgAction, CommandFactory, Parser, Subcommand};

mod cli;

/// Write default rezconfig. Path: None = ~/.rez/rezconfig.py (default), Some("bin") = next to exe, Some(p) = p.
/// Creates ~/.rez/, ~/.rez/packages/{local,int,ext,bind,pip,python} when writing to default location.
/// If target exists and force=false, returns error.
fn write_default_config(path: Option<&str>, force: bool) -> foundation::errors::Result<()> {
    let (out_path, content) = {
        let base_path = match path {
            None => {
                let home = model::platform::SystemInfo::home().ok_or_else(|| {
                    foundation::errors::RezError::Config("Cannot determine home directory".into())
                })?;
                let dir = PathBuf::from(&home).join(".rez");
                std::fs::create_dir_all(&dir).map_err(|e| {
                    foundation::errors::RezError::Config(format!("Cannot create {dir:?}: {e}"))
                })?;
                for sub in &[
                    "packages/local",
                    "packages/int",
                    "packages/ext",
                    "packages/bind",
                    "packages/pip",
                    "packages/python",
                ] {
                    let p = dir.join(sub);
                    std::fs::create_dir_all(&p).map_err(|e| {
                        foundation::errors::RezError::Config(format!(
                            "Cannot create {}: {e}",
                            p.display()
                        ))
                    })?;
                }
                dir
            }
            Some("bin") => {
                let exe = std::env::current_exe().map_err(|e| {
                    foundation::errors::RezError::Config(format!("Cannot get executable path: {e}"))
                })?;
                exe.parent()
                    .ok_or_else(|| {
                        foundation::errors::RezError::Config("No parent for executable".into())
                    })?
                    .to_path_buf()
            }
            Some(p) => {
                let expanded = if p.starts_with("~/") {
                    model::platform::SystemInfo::home()
                        .map(|h| PathBuf::from(h).join(p.trim_start_matches("~/")))
                        .unwrap_or_else(|| PathBuf::from(p))
                } else {
                    PathBuf::from(p)
                };
                if expanded.is_dir()
                    || (!expanded.exists()
                        && !expanded
                            .extension()
                            .is_some_and(|e| e == "toml" || e == "py"))
                {
                    expanded
                } else {
                    if expanded.exists() && !force {
                        return Err(foundation::errors::RezError::Config(format!(
                            "Config file already exists: {}. Use --force to overwrite.",
                            expanded.display()
                        )));
                    }
                    let content = if expanded.extension().is_some_and(|e| e == "py") {
                        model::config::RezConfig::default_config_py()
                    } else {
                        model::config::RezConfig::default_config_toml()?
                    };
                    std::fs::create_dir_all(expanded.parent().unwrap_or(std::path::Path::new(".")))
                        .map_err(|e| {
                            foundation::errors::RezError::Config(format!(
                                "Cannot create directory: {e}"
                            ))
                        })?;
                    std::fs::write(&expanded, content).map_err(|e| {
                        foundation::errors::RezError::Config(format!(
                            "Cannot write {}: {e}",
                            expanded.display()
                        ))
                    })?;
                    println!("Wrote default config to {}", expanded.display());
                    return Ok(());
                }
            }
        };

        let out_py = base_path.join("rezconfig.py");
        let content = model::config::RezConfig::default_config_py();
        (out_py, content)
    };

    if out_path.exists() && !force {
        return Err(foundation::errors::RezError::Config(format!(
            "Config file already exists: {}. Use --force to overwrite.",
            out_path.display()
        )));
    }

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            foundation::errors::RezError::Config(format!(
                "Cannot create directory {}: {e}",
                parent.display()
            ))
        })?;
    }
    std::fs::write(&out_path, content).map_err(|e| {
        foundation::errors::RezError::Config(format!("Cannot write {}: {e}", out_path.display()))
    })?;
    println!("Wrote default config to {}", out_path.display());
    Ok(())
}

const HELP_TEMPLATE: &str = "\
{about-with-newline}
{usage-heading} {usage}

{options}

Resolve (environment & context):
  env            Resolve packages and launch an interactive shell
  context        Inspect a resolved context (.rxt file)
  gui            Graphical package browser and resolver
  interpret      Interpret rex commands and output shell code
  complete       Generate shell completions for package names
  suite          Manage rez suites (tool wrappers)
  bundle         Bundle a resolved context for offline use

Query (package information):
  search         Search for packages in repositories
  view           View package information and metadata
  help-pkg       Display package help documentation
  depends        Reverse dependency lookup
  diff           Compare two package versions
  plugins        List package plugins

Development (build & release):
  build          Build the current package
  test           Run package tests
  release        Build and deploy to release repository

Repository (package operations):
  cp             Copy packages between repositories
  mv             Move packages between repositories
  rm             Remove packages from a repository
  pkg-cache      Manage the local package cache
  pkg-ignore     Ignore/unignore packages from resolves
  bind           Bind system packages into a repository
  pip            Install pip packages as rez packages
  yaml2py        Convert package.yaml to package.py format

Administration (system & tools):
  config         Query or display configuration settings
  status         Show rez system status and diagnostics
  selftest       Run rez self-tests (cargo test)
  benchmark      Run benchmarking suite for package resolves
  memcache       Manage memcache servers
  python         Run embedded Python interpreter or REPL
  forward        Execute a YAML forwarding script

Studio Setup Walkthrough:
  # 1. Check rez is working
  rez status

  # 2. Bind system packages (platform, os, arch, python, etc.)
  rez bind --quickstart

  # 3. Inspect what got bound
  rez search --sort
  rez view platform

  # 4. Create your first resolved environment
  rez env python -- python --version

  # 5. Build a package (from its source directory)
  cd my_tool && rez build --install

  # 6. Release a package to the shared repository
  rez release

  # 7. Launch a multi-package environment
  rez env maya python my_tool -- maya

  # 8. Inspect what's in the resolved context
  rez context

  # 9. Compare package versions
  rez diff maya-2024 maya-2025

See 'rez <command> --help' for details on a specific command.
";

/// Rez package manager - resolve, build, and manage software environments.
#[derive(Parser, Debug)]
#[command(name = "rez", bin_name = "rez", version)]
#[command(propagate_version = true, disable_help_subcommand = true)]
#[command(help_template = HELP_TEMPLATE)]
struct Cli {
    /// Write default rezconfig.py and exit. Path: file path, or dir (writes dir/rezconfig.py).
    /// Default (no path): creates ~/.rez/rezconfig.py and ~/.rez/packages/{local,int,ext}.
    /// Use "." for current dir, "bin" for next to rez binary. Fails if file exists unless --force.
    #[arg(long = "write-config", value_name = "PATH", num_args = 0..=1)]
    pub write_config: Option<Option<String>>,

    /// Overwrite existing config file when using --write-config
    #[arg(long, requires = "write_config")]
    pub force: bool,

    /// Verbosity: -v INFO, -vv DEBUG, -vvv TRACE
    #[arg(short = 'v', long, action = ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Redirect logging to file. No value = rez.log, or specify filename
    #[arg(long = "log", value_name = "FILE", num_args = 0..=1, global = true)]
    pub log_file: Option<Option<String>>,

    /// Machine-readable native entry point metadata for installers.
    #[arg(long, hide = true)]
    list_cli_aliases: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    // -- resolve: environment & context --
    /// Resolve packages and launch an interactive shell environment.
    #[command(name = "env")]
    Env(cli::resolve::env::EnvArgs),

    /// Inspect a resolved context (.rxt file).
    #[command(name = "context")]
    Context(cli::resolve::context::ContextArgs),

    /// Interpret rex commands and output shell code.
    #[command(name = "interpret")]
    Interpret(cli::resolve::interpret::InterpretArgs),

    /// Generate shell completions for package names.
    #[command(name = "complete")]
    Complete(cli::resolve::complete::CompleteArgs),

    /// Manage rez suites.
    #[command(name = "suite")]
    Suite(cli::resolve::suite::SuiteArgs),

    /// Bundle a resolved context for offline use.
    #[command(name = "bundle")]
    Bundle(cli::resolve::bundle::BundleArgs),

    /// Graphical package browser and resolver.
    #[cfg(feature = "gui")]
    #[command(name = "gui")]
    Gui(cli::gui::GuiArgs),

    // -- query: package information --
    /// Search for packages.
    #[command(name = "search")]
    Search(cli::query::search::SearchArgs),

    /// View package information.
    #[command(name = "view")]
    View(cli::query::view::ViewArgs),

    /// Display package help.
    #[command(name = "help-pkg", alias = "help", disable_version_flag = true)]
    HelpPkg(cli::query::help_cmd::HelpCmdArgs),

    /// Perform reverse dependency lookup.
    #[command(name = "depends")]
    Depends(cli::query::depends::DependsArgs),

    /// Compare two package versions.
    #[command(name = "diff")]
    Diff(cli::query::diff::DiffArgs),

    /// List package plugins.
    #[command(name = "plugins")]
    Plugins(cli::query::plugins::PluginsArgs),

    // -- dev: build & release --
    /// Build the current package.
    #[command(name = "build")]
    Build(cli::dev::build::BuildArgs),

    /// Run package tests.
    #[command(name = "test")]
    Test(cli::dev::test::TestArgs),

    /// Build and deploy a package to release repository.
    #[command(name = "release")]
    Release(cli::dev::release::ReleaseArgs),

    // -- repo: repository operations --
    /// Copy packages between repositories.
    #[command(name = "cp")]
    Cp(cli::repo::cp::CpArgs),

    /// Move packages between repositories.
    #[command(name = "mv")]
    Mv(cli::repo::mv::MvArgs),

    /// Remove packages from a repository.
    #[command(name = "rm")]
    Rm(cli::repo::rm::RmArgs),

    /// Manage the package cache.
    #[command(name = "pkg-cache")]
    PkgCache(cli::repo::cache::PkgCacheArgs),

    /// Ignore or unignore packages from resolves.
    #[command(name = "pkg-ignore")]
    PkgIgnore(cli::repo::pkg_ignore::PkgIgnoreArgs),

    /// Bind system packages into a repository.
    #[command(name = "bind")]
    Bind(cli::repo::bind::BindArgs),

    /// Install pip packages as rez packages.
    #[command(name = "pip")]
    Pip(cli::repo::pip::PipArgs),

    /// Convert package.yaml to package.py format.
    #[command(name = "yaml2py")]
    Yaml2py(cli::repo::yaml2py::Yaml2pyArgs),

    // -- admin: system & tools --
    /// Deploy this executable and its native CLI entry points.
    #[command(
        name = "deploy",
        after_help = "Examples:\n  rez deploy\n  rez deploy --bin-dir C:\\rez3\\Scripts\\rez\n  rez deploy --bin-dir /opt/rez/bin/rez --source-zip ./rez-rs.zip\n  rez deploy --dry-run\n  rez deploy --recover-only\n\nDefaults to the running executable's directory. Alias names follow the CLI parser.\nExisting foreign aliases are refused; originals are backed up and interrupted\nactivation can be recovered. An omitted source archive is retained."
    )]
    Deploy(cli::admin::deploy::DeployArgs),

    /// Query or display configuration settings.
    #[command(name = "config")]
    Config(cli::admin::config::ConfigArgs),

    /// Show rez system status.
    #[command(name = "status")]
    Status(cli::admin::status::StatusArgs),

    /// Run rez self-tests.
    #[command(name = "selftest")]
    Selftest(cli::admin::selftest::SelftestArgs),

    /// Run benchmarking suite for package resolves.
    #[command(name = "benchmark")]
    Benchmark(cli::admin::benchmark::BenchmarkArgs),

    /// Manage memcache servers.
    #[command(name = "memcache")]
    Memcache(cli::admin::memcache::MemcacheArgs),

    /// Run embedded Python interpreter, script, or REPL.
    #[command(name = "python", disable_version_flag = true)]
    Python(cli::admin::python::PythonArgs),

    /// Execute a forwarding script.
    #[command(name = "forward")]
    Forward(cli::admin::forward::ForwardArgs),
}

fn main() -> std::process::ExitCode {
    let command = Cli::command();
    let argv = cli::normalize_argv(&command, std::env::args_os().collect());
    if argv.len() == 2
        && argv[1] == "--list-cli-aliases"
        && Cli::try_parse_from(&argv).is_ok_and(|cli| cli.list_cli_aliases)
    {
        match serde_json::to_string(&cli::aliases(&command)) {
            Ok(aliases) => println!("{aliases}"),
            Err(error) => {
                eprintln!("rez: error: {error}");
                return std::process::ExitCode::FAILURE;
            }
        }
        return std::process::ExitCode::SUCCESS;
    }
    // Python reentry uses its own parser; normal Rez help/errors do not initialize it.
    if cli::admin::python::is_reentry(&command, &argv) {
        if let Err(error) = python_runtime::register_cli() {
            eprintln!("rez: error: {error}");
            return std::process::ExitCode::FAILURE;
        }
        return python_runtime::run_cli().unwrap_or_else(|error| {
            eprintln!("rez: error: {error}");
            std::process::ExitCode::FAILURE
        });
    }
    let python_index = cli::admin::python::command_index(&command, &argv);
    let mut cli = if let Some(index) = python_index {
        Cli::parse_from(&argv[..=index])
    } else {
        Cli::parse_from(&argv)
    };
    // Deployment and its help/errors are handled before Python registration.
    if let Some(Commands::Deploy(args)) = &cli.command {
        return match cli::admin::deploy::run(args, &command) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("rez: error: {error}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    if let Err(error) = python_runtime::register_cli() {
        eprintln!("rez: error: {error}");
        return std::process::ExitCode::FAILURE;
    }
    // Registration only supplies executable identity; snapshot mapping may create the VM.
    if let Some(Commands::PkgCache(args)) = &cli.command {
        if let cli::repo::cache::PkgCacheCommand::Worker {
            request: Some(request),
            ..
        } = &args.command
        {
            let result = args
                .dir
                .as_deref()
                .ok_or_else(|| {
                    foundation::errors::RezError::PackageCache(
                        "A request worker requires --dir".into(),
                    )
                })
                .and_then(|directory| {
                    repository::package::cache::PackageCache::initialize_worker(
                        std::path::Path::new(directory),
                        request,
                    )
                });
            if let Err(error) = result {
                eprintln!("rez: error: {error}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }

    if let (Some(index), Some(Commands::Python(args))) = (python_index, &mut cli.command) {
        args.arguments = argv[index + 1..].to_vec();
    }

    // Initialize logging early (before any component that might log)
    let log_file = if cli.log_file.is_some() {
        Some(cli.log_file.clone().flatten())
    } else {
        None
    };
    if let Err(e) = foundation::logging::init(cli.verbose, log_file) {
        eprintln!("rez: warning: failed to init logging: {e}");
    }

    if let Some(path_opt) = cli.write_config {
        let result = write_default_config(path_opt.as_deref(), cli.force);
        if let Err(e) = result {
            eprintln!("rez: error: {e}");
            std::process::exit(1);
        }
        return std::process::ExitCode::SUCCESS;
    }

    let command = match cli.command {
        Some(c) => c,
        None => {
            eprintln!("rez: error: a subcommand is required");
            eprintln!("Use 'rez --help' for usage information.");
            std::process::exit(1);
        }
    };

    let result = match command {
        // resolve
        Commands::Env(ref args) => cli::resolve::env::run(args),
        Commands::Context(ref args) => {
            cli::resolve::context::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::Interpret(ref args) => {
            cli::resolve::interpret::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::Complete(ref args) => {
            cli::resolve::complete::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::Suite(ref args) => {
            cli::resolve::suite::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::Bundle(ref args) => {
            cli::resolve::bundle::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        #[cfg(feature = "gui")]
        Commands::Gui(ref args) => cli::gui::run(args).map(|()| std::process::ExitCode::SUCCESS),
        // query
        Commands::Search(ref args) => {
            cli::query::search::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::View(ref args) => {
            cli::query::view::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::HelpPkg(ref args) => {
            cli::query::help_cmd::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::Depends(ref args) => {
            cli::query::depends::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::Diff(ref args) => {
            cli::query::diff::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::Plugins(ref args) => {
            cli::query::plugins::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        // dev
        Commands::Build(ref args) => {
            cli::dev::build::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::Test(ref args) => {
            cli::dev::test::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::Release(ref args) => {
            cli::dev::release::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        // repo
        Commands::Cp(ref args) => {
            cli::repo::cp::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::Mv(ref args) => {
            cli::repo::mv::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::Rm(ref args) => {
            cli::repo::rm::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::PkgCache(ref args) => {
            cli::repo::cache::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::PkgIgnore(ref args) => {
            cli::repo::pkg_ignore::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::Bind(ref args) => {
            cli::repo::bind::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::Pip(ref args) => {
            cli::repo::pip::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::Yaml2py(ref args) => {
            cli::repo::yaml2py::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        // admin
        Commands::Deploy(ref args) => {
            cli::admin::deploy::run(args, &Cli::command()).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::Config(ref args) => {
            cli::admin::config::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::Status(ref args) => {
            cli::admin::status::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::Selftest(ref args) => {
            cli::admin::selftest::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::Benchmark(ref args) => {
            cli::admin::benchmark::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::Memcache(ref args) => {
            cli::admin::memcache::run(args).map(|()| std::process::ExitCode::SUCCESS)
        }
        Commands::Python(ref args) => cli::admin::python::run(args, None),
        Commands::Forward(ref args) => cli::admin::forward::run(args),
    };

    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("rez: error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod cli_tests {
    use super::{Cli, Commands};
    use clap::{CommandFactory, Parser};

    #[test]
    fn package_help_alias_owns_help_and_global_help_stays_available() {
        Cli::command().debug_assert();
        let parsed = Cli::try_parse_from(["rez", "help", "python"]).unwrap();
        assert!(matches!(parsed.command, Some(Commands::HelpPkg(_))));
        let global_help = Cli::try_parse_from(["rez", "--help"]).unwrap_err();
        assert_eq!(global_help.kind(), clap::error::ErrorKind::DisplayHelp);
    }

    #[test]
    fn env_parent_inheritance_matches_rez_and_keeps_explicit_empty_mode() {
        for (args, inherited) in [
            (vec!["rez", "env"], true),
            (vec!["rez", "env", "--inherited"], true),
            (vec!["rez", "env", "--inherited=false"], false),
        ] {
            let parsed = Cli::try_parse_from(args).unwrap();
            let Some(Commands::Env(env)) = parsed.command else {
                panic!("expected env command")
            };
            assert_eq!(env.inherited, inherited);
        }
    }

    #[test]
    fn cache_commands_own_package_versions() {
        for args in [
            vec![
                "rez",
                "pkg-cache",
                "add",
                "payload",
                "--name",
                "python",
                "--version",
                "3.10",
            ],
            vec!["rez", "pkg-cache", "remove", "python", "3.10"],
        ] {
            let parsed = Cli::try_parse_from(args).unwrap();
            assert!(matches!(parsed.command, Some(Commands::PkgCache(_))));
        }
        let version = Cli::try_parse_from(["rez", "--version"]).unwrap_err();
        assert_eq!(version.kind(), clap::error::ErrorKind::DisplayVersion);
    }

    #[test]
    fn verbosity_has_one_global_definition() {
        let command = Cli::command();
        let global_ids: Vec<_> = command
            .get_arguments()
            .filter(|arg| arg.is_global_set())
            .map(|arg| arg.get_id().clone())
            .collect();
        for child in command.get_subcommands() {
            for arg in child.get_arguments() {
                assert!(
                    !global_ids.contains(arg.get_id()),
                    "{} redeclares global argument {}",
                    child.get_name(),
                    arg.get_id()
                );
            }
        }
    }

    #[test]
    fn verbosity_reaches_every_consumer_before_and_after_subcommand() {
        let cases: &[&[&str]] = &[
            &["env"],
            &["context"],
            &["suite"],
            &["bundle", "context.rxt", "destination"],
            &["test"],
            &["release"],
            &["cp"],
            &["mv", "probe-1", "--dest-path", "destination"],
            &["bind", "--list"],
            &["selftest"],
            &["memcache"],
        ];
        for tokens in cases {
            for (flag, expected) in [
                (None, 0),
                (Some("-v"), 1),
                (Some("-vv"), 2),
                (Some("--verbose"), 1),
            ] {
                for before in [true, false] {
                    let mut argv = vec!["rez"];
                    if before {
                        argv.extend(flag);
                    }
                    argv.extend_from_slice(tokens);
                    if !before {
                        argv.extend(flag);
                    }
                    let cli = Cli::try_parse_from(&argv)
                        .unwrap_or_else(|error| panic!("{argv:?}: {error}"));
                    assert_eq!(cli.verbose, expected, "{argv:?}");
                    let consumer = match cli.command.unwrap() {
                        Commands::Env(args) => args.verbose,
                        Commands::Context(args) => args.verbose,
                        Commands::Suite(args) => args.verbose,
                        Commands::Bundle(args) => args.verbose,
                        Commands::Test(args) => args.verbose,
                        Commands::Release(args) => args.verbose,
                        Commands::Cp(args) => args.verbose,
                        Commands::Mv(args) => args.verbose,
                        Commands::Bind(args) => args.verbose,
                        Commands::Selftest(args) => args.verbose,
                        Commands::Memcache(args) => args.verbose,
                        _ => unreachable!(),
                    };
                    assert_eq!(consumer, expected, "{argv:?}");
                }
            }
        }
    }
}
