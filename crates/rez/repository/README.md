# repository

Repository access and high-level package APIs. This crate owns discovery, search,
creation, copy/move/remove, caching, binding, and locked publication. Canonical
package types, filtering, ordering, and loading live in `model`; package test
execution lives in `resolve`. Consumers import canonical types from `model::package`
and repository operations from this crate. See the [workspace architecture](../../../docs/mdbook/src/architecture-crates.md).

## Files

| File | Description |
|------|-------------|
| src/lib.rs, src/package/mod.rs | Crate and package module declarations; exports repository-owned provider, repository, and discovery APIs. |
| src/repository.rs | Filesystem repositories, metadata loading, publication, persistent locks, and repository management. |
| src/provider.rs | `PackageProvider`, filesystem/memory implementations, candidates, and source provenance shared with the solver. |
| ../model/src/package/core.rs | Core types: `Package` (metadata, deps, commands, variants), `Variant` (specific variant of a package), `PackageFamily`, `DeveloperPackage` (filesystem-backed). `PackageRequest` type alias for `Requirement`. `Package::from_data()` constructs from JSON-compatible HashMap. |
| discover.rs | High-level package API: `iter_package_families()`, `iter_packages()`, `get_package()`, `get_latest_package()`, `get_developer_package()`, `get_completions()`. Searches across `CONFIG.packages_path` repositories. `PackageSearchPath` wrapper for multi-repo search. |
| search.rs | Package search engine: `PackageSearcher` with glob/regex/substring matching, `ResourceSearchResult`, `MatchType` enum. Also `get_reverse_dependencies()` for reverse dep lookups. |
| ../model/src/package/filter.rs | Package filtering: `Rule` enum (Glob, Family, Range, Regex), `PackageFilter` (include/exclude rule lists), `PackageFilterList` (ordered chain of filters). Used by the resolver to exclude packages. |
| ../model/src/package/order.rs | Package ordering: `PackageOrder` trait (dynamic dispatch), `PerFamilyOrder`, `TimestampOrder`, `VersionSplitOrder`, `SortOrder` enum. `PackageOrderList` applies orderers in sequence. Controls which version the resolver picks first. |
| bind.rs | System software binding: `BindInfo` struct, `BindFormat` enum. Built-in detectors: `detect_platform()`, `detect_arch()`, `detect_os()`, `detect_python()`, `detect_pip()`, `detect_setuptools()`, `detect_rez()`, `detect_cmake()`, `detect_gcc()`. Writes `package.py` or `package.yaml`. |
| help.rs | Package help system: `HelpEntry` struct with label + URI. Extracts help from package `help` attribute (string or list-of-pairs). Opens URLs in browser or runs shell commands. |
| maker.rs | Package builder: `PackageMaker` (consuming builder pattern). Sets name, version, description, requires, tools, commands, variants, etc. Outputs JSON-compatible data or writes package files directly. |
| ops.rs | Package filesystem operations: `copy_package()`, `move_package()`, `remove_package()`. `CopyOptions` / `CopyResult` structs. Also `copy_dir_contents()` helper. |
| ../resolve/src/package/test.rs | Package test runner: `PackageTestRunner`, `TestRunOn` enum (Default, PreInstall, PostInstall, PreRelease, PostRelease, Explicit). Runs tests defined in the package's `tests` attribute. |
| cache.rs | Package cache: `PackageCache` manages local copies of variant payloads. `CacheStatus` enum (Found, Copying, CopyStalled, Pending, NotFound, Removed). Cache layout: `<root>/<name>/<version>/<hash>/<slot>/`. |

## Architecture

Unqualified file names in the table refer to `src/package/`. In the diagram below,
`core.rs`, `filter.rs`, and `order.rs` are model-owned; `test.rs` is resolve-owned.

```
discover.rs  -- "find packages"
    |
    v
core.rs      -- Package / Variant / PackageFamily types
    |
    ├── filter.rs   -- "should this package be included?"
    ├── order.rs    -- "in what order should versions be tried?"
    ├── search.rs   -- "search by pattern across repos"
    |
    ├── maker.rs    -- "create new package definitions"
    ├── ops.rs      -- "copy / move / remove packages"
    ├── bind.rs     -- "bind system software as packages"
    ├── help.rs     -- "open package help"
    ├── test.rs     -- "run package tests"
    └── cache.rs    -- "local variant payload cache"
```

Data flows from discovery through filtering and ordering into the resolver.
The `Package::from_data()` constructor accepts a `HashMap<String, serde_json::Value>`,
which is produced by `serialise.rs` (from package.py via RustPython, or from
package.yaml/toml via serde).

## Key Types / Traits

| Type | Description |
|------|-------------|
| `Package` | Full package metadata: name, version, requires, variants, commands, help, tools, etc. |
| `Variant` | A specific variant of a package with `variant_requires` and computed `root` path. |
| `PackageFamily` | Collection of all versions of a named package. |
| `DeveloperPackage` | Package loaded from a source directory with filesystem path tracking. |
| `PackageRequest` | Type alias for `Requirement` -- used in resolver request contexts. |
| `PackageOrder` | Trait for version ordering strategies (dynamic dispatch via `Box<dyn PackageOrder>`). |
| `PackageFilter` | Include/exclude filter with `Rule` matching. |
| `PackageSearcher` | Configurable multi-repo package search engine. |
| `PackageMaker` | Builder pattern for constructing package definitions. |
| `PackageCache` | Local cache for variant payloads with status tracking. |

## Usage

```rust
use model::package::{Package, PackageRequest, PackageFilter};
use repository::package::discover::{iter_packages, get_latest_package};
use repository::package::search::PackageSearcher;
use repository::package::maker::PackageMaker;
use version::Version;

// Find packages
let packages = iter_packages("maya", None, None)?;

// Create a package
let maker = PackageMaker::new("foo")
    .version(Version::new("1.0.0")?)
    .description("A foo package");
```
