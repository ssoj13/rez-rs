//! CLI command modules for the rez binary.
//!
//! Organized into five groups:
//! - `resolve` -- environment resolution, context inspection, shell interaction
//! - `query`   -- read-only package information lookups
//! - `dev`     -- build, test, release workflow
//! - `repo`    -- repository mutations (copy, move, remove, bind, cache)
//! - `admin`   -- system diagnostics, configuration, tools

pub mod admin;
pub mod dev;
#[cfg(feature = "gui")]
pub mod gui;
pub mod query;
pub mod repo;
pub mod resolve;
pub mod util;

/// Derive native entry points from the same metadata that drives argument parsing.
pub fn aliases(command: &clap::Command) -> std::collections::BTreeMap<String, String> {
    let mut aliases = std::collections::BTreeMap::new();
    for child in command.get_subcommands() {
        for name in std::iter::once(child.get_name()).chain(child.get_all_aliases()) {
            aliases.insert(format!("rez-{name}"), child.get_name().to_owned());
        }
    }
    // Original Rez's three entry points that do not follow rez-<subcommand>.
    aliases.insert("rezolve".into(), String::new());
    for (name, subcommand) in [("_rez-complete", "complete"), ("_rez_fwd", "forward")] {
        if command.find_subcommand(subcommand).is_some() {
            aliases.insert(name.into(), subcommand.into());
        }
    }
    aliases
}

/// Route native alias invocations through the existing parser and dispatcher.
pub fn normalize_argv(
    command: &clap::Command,
    mut argv: Vec<std::ffi::OsString>,
) -> Vec<std::ffi::OsString> {
    let Some(name) = argv
        .first()
        .and_then(|path| std::path::Path::new(path).file_name())
        .and_then(|name| name.to_str())
    else {
        return argv;
    };
    #[cfg(windows)]
    let name = name.to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(name.as_ref());
    if let Some(subcommand) = aliases(command).get(name) {
        if !subcommand.is_empty() {
            argv.insert(1, subcommand.as_str().into());
        }
    }
    argv
}

#[cfg(test)]
mod alias_tests {
    use super::{aliases, normalize_argv};
    use clap::Command;
    use std::ffi::OsString;

    fn metadata() -> Command {
        Command::new("rez")
            .subcommand(Command::new("pip"))
            .subcommand(Command::new("python"))
            .subcommand(Command::new("help-pkg").alias("help"))
            .subcommand(Command::new("complete"))
            .subcommand(Command::new("forward"))
    }

    #[test]
    fn names_and_targets_follow_parser_metadata() {
        let aliases = aliases(&metadata());
        assert_eq!(aliases["rez-pip"], "pip");
        assert_eq!(aliases["rez-help"], "help-pkg");
        assert_eq!(aliases["_rez-complete"], "complete");
        assert_eq!(aliases["_rez_fwd"], "forward");
        assert_eq!(aliases["rezolve"], "");
        assert!(!aliases.contains_key("rez-gui"));
    }

    #[test]
    fn native_aliases_preserve_every_argument() {
        for program in ["rez-pip", "rez-pip.exe"] {
            let argv = [program, "-i", "--prefix", "space directory", "-negative"]
                .map(OsString::from)
                .to_vec();
            let normalized = normalize_argv(&metadata(), argv.clone());
            assert_eq!(normalized[0], argv[0]);
            assert_eq!(normalized[1], "pip");
            assert_eq!(normalized[2..], argv[1..]);
        }
    }

    #[test]
    fn primary_unknown_and_python_runtime_names_are_unchanged() {
        for program in [
            "rez",
            "rez.exe",
            "python",
            "python.exe",
            "rez-unknown",
            "rezolve",
        ] {
            let argv = vec![OsString::from(program), OsString::from("-c")];
            assert_eq!(normalize_argv(&metadata(), argv.clone()), argv);
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_arguments_are_preserved() {
        use std::os::unix::ffi::OsStringExt;
        let arg = OsString::from_vec(vec![0xff]);
        let result = normalize_argv(&metadata(), vec!["rez-python".into(), arg.clone()]);
        assert_eq!(
            result,
            vec![OsString::from("rez-python"), "python".into(), arg]
        );
    }
}
