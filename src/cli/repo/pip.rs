// SPDX-License-Identifier: Apache-2.0

//! Pip CLI parsing; native orchestration and conversion live in the shared library.

use build_system::pip::{self, Options, VariantPolicy};
use clap::Args;
use foundation::errors::{Result, RezError};
use model::config::CONFIG;
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct PipArgs {
    /// Package requirements, wheels, archives, URLs or source directories.
    #[arg(value_name = "PACKAGE", required = true, num_args = 1..)]
    pub packages: Vec<String>,
    /// Install the requested packages.
    #[arg(short = 'i', long)]
    pub install: bool,
    /// Publish to the configured release repository.
    #[arg(short = 'r', long, conflicts_with = "local")]
    pub release: bool,
    /// Install to the local repository (the default).
    #[arg(long, conflicts_with_all = ["release", "pre_release"])]
    pub local: bool,
    /// Override the destination package repository.
    #[arg(short = 'p', long, value_name = "PATH")]
    pub prefix: Option<PathBuf>,
    /// Python major.minor line; defaults to configuration, then latest.
    #[arg(long = "python-version", value_name = "VERSION")]
    pub py_ver: Option<String>,
    /// Current interpreter variants, or verified portable packages without variants.
    #[arg(long, default_value = "current")]
    pub variant_policy: VariantPolicy,
    /// Additional Rez Python range for portable packages, intersected with Requires-Python.
    #[arg(long, value_name = "RANGE")]
    pub python_requires: Option<String>,
    /// Resolve a particular pip version from Rez packages.
    #[arg(long = "pip-version", value_name = "VERSION")]
    pub pip_ver: Option<String>,
    /// Do not install dependencies.
    #[arg(short = 'n', long, conflicts_with = "deps")]
    pub no_deps: bool,
    /// Install dependencies even when disabled by configuration.
    #[arg(long, conflicts_with = "no_deps")]
    pub deps: bool,
    /// Release a non-final version, replacing its selected variant and disabling caching.
    #[arg(short = 'a', long, conflicts_with = "local")]
    pub pre_release: bool,
    /// Build source directories into wheels before installation.
    #[arg(long)]
    pub build: bool,
    /// Upload wheels with twine before release publication.
    #[arg(long, conflicts_with = "skip_pypi_upload")]
    pub pypi_upload: bool,
    #[arg(long, hide = true, conflicts_with = "pypi_upload")]
    pub skip_pypi_upload: bool,
    /// Remaining options passed to pip install.
    #[arg(short = 'e', long, num_args = 0.., allow_hyphen_values = true)]
    pub extra: Vec<String>,
    /// Additional pip install options after --.
    #[arg(last = true, value_name = "PIP_ARGS")]
    pub trailing_extra: Vec<String>,
}

pub fn run(args: &PipArgs) -> Result<()> {
    if !args.install && !args.release && !args.pre_release && !args.build {
        return Err(RezError::Build(
            "Expected --install, --release, --pre-release or --build".into(),
        ));
    }
    if args.pypi_upload && !args.release && !args.pre_release {
        return Err(RezError::Build(
            "--pypi-upload requires --release or --pre-release".into(),
        ));
    }
    let mut extra = args.extra.clone();
    extra.extend(args.trailing_extra.iter().cloned());
    pip::install(
        &Options {
            packages: args.packages.clone(),
            release: args.release,
            local: args.local,
            prefix: args.prefix.clone(),
            python_version: args.py_ver.clone(),
            pip_version: args.pip_ver.clone(),
            no_deps: args.no_deps,
            deps: args.deps,
            pre_release: args.pre_release,
            build: args.build,
            pypi_upload: args.pypi_upload,
            extra,
            variant_policy: args.variant_policy,
            python_requires: args.python_requires.clone(),
        },
        &CONFIG,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        pip: PipArgs,
    }

    #[test]
    fn variant_policy_and_floor_are_explicit() {
        let parsed = Cli::try_parse_from([
            "rez-pip",
            "-i",
            "--variant-policy",
            "none",
            "--python-requires",
            "3.10+",
            "distlib==0.4.3",
        ])
        .unwrap();
        assert_eq!(parsed.pip.variant_policy, VariantPolicy::None);
        assert_eq!(parsed.pip.python_requires.as_deref(), Some("3.10+"));
        assert!(
            Cli::try_parse_from(["rez-pip", "-i", "--variant-policy", "auto", "distlib"]).is_err()
        );
    }

    #[test]
    fn foundation_and_remainder_forms_parse() {
        let parsed = Cli::try_parse_from([
            "rez-pip",
            "-i",
            "--prefix",
            "repo/pip",
            "--python-version",
            "3.13",
            "pythonnet==3.1.0",
        ])
        .unwrap();
        assert_eq!(parsed.pip.py_ver.as_deref(), Some("3.13"));
        assert!(!parsed.pip.release);
        let parsed = Cli::try_parse_from([
            "rez-pip",
            "-i",
            "foo",
            "-e",
            "--no-index",
            "--find-links",
            "cache",
        ])
        .unwrap();
        assert_eq!(parsed.pip.extra, ["--no-index", "--find-links", "cache"]);
        let parsed = Cli::try_parse_from(["rez-pip", "-r", "foo", "--", "--no-index"]).unwrap();
        assert_eq!(parsed.pip.trailing_extra, ["--no-index"]);
    }
}
