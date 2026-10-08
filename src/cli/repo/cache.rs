// SPDX-License-Identifier: Apache-2.0

//! `rez pkg-cache` - manage local package cache.

use std::path::PathBuf;

use clap::{Args, Subcommand};

use foundation::errors::{Result, RezError};
use model::config::CONFIG;
use repository::package::cache::{CacheStatus, CachedVariantInfo, PackageCache, VariantHandle};

/// Manipulate the package cache.
#[derive(Args, Debug)]
pub struct PkgCacheArgs {
    #[command(subcommand)]
    pub command: PkgCacheCommand,

    /// Package cache directory (default: config cache_packages_path)
    #[arg(long = "dir", global = true, value_name = "DIR")]
    pub dir: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum PkgCacheCommand {
    /// List cached packages
    List {
        /// Columns to display
        #[arg(short = 'c', long, num_args = 1.., value_name = "COL",
              default_values_t = vec![
                  "status".to_string(),
                  "package".to_string(),
                  "cache_path".to_string(),
              ])]
        columns: Vec<String>,
    },

    /// Add a variant to the cache
    #[command(disable_version_flag = true)]
    Add {
        /// Variant source root path
        #[arg(value_name = "PATH")]
        path: String,

        /// Package name
        #[arg(long, value_name = "NAME")]
        name: String,

        /// Package version
        #[arg(long, value_name = "VERSION")]
        version: String,

        /// Variant index
        #[arg(long, value_name = "INDEX")]
        index: Option<usize>,

        /// Repository URI
        #[arg(long, value_name = "REPO", default_value = "filesystem")]
        repository: String,

        /// Select an exact combined-family definition (requires a concrete repository)
        #[arg(long, value_parser = ["py", "yaml"], value_name = "EXT")]
        source_ext: Option<String>,

        /// Force add even if not cacheable
        #[arg(short = 'f', long)]
        force: bool,
    },

    /// Remove a variant from the cache
    #[command(disable_version_flag = true)]
    Remove {
        /// Package name
        #[arg(value_name = "NAME")]
        name: String,

        /// Package version
        #[arg(value_name = "VERSION")]
        version: String,

        /// Variant index
        #[arg(long, value_name = "INDEX")]
        index: Option<usize>,

        /// Repository URI
        #[arg(long, value_name = "REPO", default_value = "filesystem")]
        repository: String,

        /// Select an exact combined-family definition
        #[arg(long, value_parser = ["py", "yaml"], value_name = "EXT")]
        source_ext: Option<String>,
    },

    /// Clean old/stalled cache entries
    Clean {
        /// Max age in days (0 = no age limit, only clean stalled)
        #[arg(long = "max-age", default_value_t = 0, value_name = "DAYS")]
        max_age: u32,
    },

    /// Drain durable native cache requests.
    #[command(hide = true)]
    Worker {
        /// Wait for another worker that owns a pending request.
        #[arg(long)]
        wait: bool,
        /// Exact durable request processed with its original typed configuration.
        #[arg(long, value_name = "FILE")]
        request: Option<PathBuf>,
    },

    /// Show cache status/statistics
    Status,
}

/// Resolve cache directory from args or config.
fn cache_path(args: &PkgCacheArgs) -> Result<PathBuf> {
    if let Some(ref dir) = args.dir {
        return Ok(model::config::RezConfig::expand_path(dir).to_os());
    }
    let cfg = &*CONFIG;
    cfg.cache_packages_path
        .as_deref()
        .map(|path| model::config::RezConfig::expand_path(path).to_os())
        .ok_or_else(|| {
            RezError::PackageCache(
                "Cache directory not specified and cache_packages_path not configured".into(),
            )
        })
}

/// Status string for display.
fn status_label(status: &CacheStatus) -> &'static str {
    match status {
        CacheStatus::Found(_) => "cached",
        CacheStatus::Copying(_) => "copying",
        CacheStatus::CopyStalled(_) => "stalled",
        CacheStatus::Pending => "pending",
        CacheStatus::NotFound => "missing",
        CacheStatus::Removed => "removed",
        CacheStatus::Skipped => "skipped",
    }
}

/// Sort key for variant info: status priority, then name.
fn sort_key(info: &CachedVariantInfo) -> (u8, String) {
    let priority = match &info.status {
        CacheStatus::Found(_) => 0,
        CacheStatus::Copying(_) => 1,
        CacheStatus::CopyStalled(_) => 2,
        CacheStatus::Pending => 3,
        _ => 4,
    };
    (priority, info.handle.name.clone())
}

fn cmd_list(cache: &PackageCache, columns: &[String]) -> Result<()> {
    let mut entries = cache.get_variants()?;
    entries.sort_by_key(sort_key);

    if entries.is_empty() {
        eprintln!("No cached packages.");
        return Ok(());
    }

    // Print header
    let valid_cols = ["status", "package", "cache_path"];
    let cols: Vec<&str> = columns
        .iter()
        .map(|s| s.as_str())
        .filter(|c| valid_cols.contains(c))
        .collect();

    // Header
    let header: Vec<String> = cols.iter().map(|c| c.replace('_', " ")).collect();
    println!("{}", header.join("  "));
    let underline: Vec<String> = cols.iter().map(|c| "-".repeat(c.len())).collect();
    println!("{}", underline.join("  "));

    for info in &entries {
        let mut row = Vec::new();
        for col in &cols {
            match *col {
                "status" => row.push(format!("{:<8}", status_label(&info.status))),
                "package" => row.push(info.handle.qualified_name()),
                "cache_path" => {
                    let path = if info.cache_path.as_os_str().is_empty() {
                        "-".to_string()
                    } else {
                        info.cache_path.display().to_string()
                    };
                    row.push(path);
                }
                _ => {}
            }
        }
        println!("{}", row.join("  "));
    }

    Ok(())
}

fn cmd_add(
    cache: &PackageCache,
    path: &str,
    request: &VariantHandle,
    force: bool,
    source_ext: Option<&str>,
) -> Result<()> {
    use repository::{FilesystemPackageProvider, PackageProvider};
    let name = request.name.as_str();
    let version = request.version.as_str();
    let index = request.index;
    let repository = request.repository.as_str();
    let source_root = std::fs::canonicalize(path)?;
    let paths = if repository == "filesystem" {
        CONFIG.expanded_packages_path_os()
    } else {
        let location = repository.strip_prefix("filesystem@").unwrap_or(repository);
        vec![model::config::RezConfig::expand_path(location).to_os()]
    };
    let provider = FilesystemPackageProvider::from_paths(&paths)?;
    let version = version::Version::new(version)?;
    let candidates = if let Some(ext) = source_ext {
        use repository::{ResourceHandle, ResourceHandleKey, ResourceHandleVariables};
        if repository == "filesystem" {
            return Err(RezError::PackageCache(
                "--source-ext requires a concrete --repository path".into(),
            ));
        }
        let location = repository.strip_prefix("filesystem@").unwrap_or(repository);
        let location = model::config::RezConfig::expand_path(location).to_os();
        let handle = ResourceHandle {
            key: ResourceHandleKey::FilesystemVariantCombined,
            variables: ResourceHandleVariables {
                repository_type: "filesystem".into(),
                location: location.to_string_lossy().into_owned(),
                name: name.into(),
                version: (!version.is_empty()).then(|| version.to_string()),
                index,
                ext: Some(ext.into()),
            },
        };
        vec![provider.get_candidate_for_handle(&handle)?]
    } else {
        provider.get_all_candidates(name)?
    };
    let mut variants = Vec::new();
    for candidate in candidates {
        if candidate.package.version != version {
            continue;
        }
        match index {
            Some(index) if index >= candidate.package.variants.len() => continue,
            None if candidate.package.has_variants() => continue,
            _ => {}
        }
        let variant = candidate.into_variant(index, false)?;
        if let Some(root) = variant.variant.root.as_deref() {
            if std::fs::canonicalize(root)? == source_root {
                variants.push(variant);
            }
        }
    }
    if variants.len() != 1 {
        return Err(RezError::PackageCache(format!(
            "Expected one repository variant for {}, found {}",
            source_root.display(),
            variants.len()
        )));
    }
    let variant = variants.pop().expect("validated length");

    println!(
        "Adding variant {}-{} to cache at {}...",
        name,
        version,
        cache.path.display()
    );

    let (dest, status) = cache.add_variant(&variant, force, Some(&CONFIG))?;

    match &status {
        CacheStatus::Found(_) => println!("Already cached: {}", dest.display()),
        CacheStatus::Copying(_) => {
            eprintln!("Warning: another process is copying to: {}", dest.display())
        }
        CacheStatus::Skipped => eprintln!("Warning: cache rejected variant (size limit)"),
        _ => println!("Cached to: {}", dest.display()),
    }

    Ok(())
}

fn cmd_remove(
    cache: &PackageCache,
    name: &str,
    version: &str,
    index: Option<usize>,
    repository: &str,
    source_ext: Option<&str>,
) -> Result<()> {
    model::serialise::validate_rez_package_path(name, Some(version))?;
    let matches: Vec<_> = cache
        .get_variants()?
        .into_iter()
        .filter(|entry| {
            entry.handle.name == name
                && entry.handle.version == version
                && entry.handle.index == index
                && source_ext.is_none_or(|ext| {
                    entry
                        .handle
                        .resource
                        .as_ref()
                        .and_then(|handle| handle.variables.ext.as_deref())
                        == Some(ext)
                })
                && (entry.handle.repository == repository
                    || (repository == "filesystem"
                        && entry.handle.repository.starts_with("filesystem@"))
                    || entry.handle.repository == format!("filesystem@{}", repository))
        })
        .collect();
    if matches.len() > 1 {
        return Err(RezError::PackageCache(
            "Several cache entries match; specify the exact repository/source identity".into(),
        ));
    }
    let handle = matches
        .into_iter()
        .next()
        .map(|entry| entry.handle)
        .unwrap_or_else(|| VariantHandle::new(name, version, index, repository));

    println!("Removing variant {}-{} from cache...", name, version);

    let status = cache.remove_variant(&handle)?;

    match &status {
        CacheStatus::NotFound => eprintln!("Error: variant not found in cache"),
        CacheStatus::Copying(_) => {
            eprintln!("Warning: variant is currently being cached by another process")
        }
        CacheStatus::Removed => println!("Variant removed."),
        _ => println!("Done (status: {}).", status_label(&status)),
    }

    Ok(())
}

fn cmd_clean(cache: &PackageCache, max_age: u32) -> Result<()> {
    let removed = cache.clean(max_age, None, None)?;
    if removed > 0 {
        println!("{} cache entries cleaned.", removed);
    } else {
        println!("Cache is clean, nothing to remove.");
    }
    Ok(())
}

fn cmd_status(cache: &PackageCache) -> Result<()> {
    let entries = cache.get_variants()?;

    let mut cached = 0u32;
    let mut copying = 0u32;
    let mut stalled = 0u32;
    let mut pending = 0u32;

    for info in &entries {
        match &info.status {
            CacheStatus::Found(_) => cached += 1,
            CacheStatus::Copying(_) => copying += 1,
            CacheStatus::CopyStalled(_) => stalled += 1,
            CacheStatus::Pending => pending += 1,
            _ => {}
        }
    }

    println!("Package cache: {}", cache.path.display());
    println!("  Cached:  {}", cached);
    println!("  Copying: {}", copying);
    println!("  Stalled: {}", stalled);
    println!("  Pending: {}", pending);
    println!("  Total:   {}", entries.len());

    Ok(())
}

pub fn run(args: &PkgCacheArgs) -> Result<()> {
    let path = cache_path(args)?;
    let cache = PackageCache::new(&path)?;

    match &args.command {
        PkgCacheCommand::List { columns } => cmd_list(&cache, columns),
        PkgCacheCommand::Add {
            path,
            name,
            version,
            index,
            repository,
            force,
            source_ext,
        } => cmd_add(
            &cache,
            path,
            &VariantHandle::new(name, version, *index, repository),
            *force,
            source_ext.as_deref(),
        ),
        PkgCacheCommand::Remove {
            name,
            version,
            index,
            repository,
            source_ext,
        } => cmd_remove(
            &cache,
            name,
            version,
            *index,
            repository,
            source_ext.as_deref(),
        ),
        PkgCacheCommand::Clean { max_age } => cmd_clean(&cache, *max_age),
        PkgCacheCommand::Status => cmd_status(&cache),
        PkgCacheCommand::Worker { wait, request } => {
            cache.run_pending(*wait, request.as_deref()).map(|_| ())
        }
    }
}
