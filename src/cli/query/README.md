# cli/query/

Read-only commands for querying package information across repositories.
None of these commands modify packages or repositories -- they only read and display data.

## Files

| File | Description |
|------|-------------|
| mod.rs | Module declarations for all six query commands. |
| search.rs | `rez search` -- Search for packages by name pattern (glob, regex, substring). Supports `--latest`, `--no-versions`, `--reverse` (reverse dependency lookup), and `--format json`. |
| view.rs | `rez view` -- View detailed package information: metadata, dependencies, variants, commands, help entries, changelog. |
| help_cmd.rs | `rez help-pkg` -- Display package help entries. Opens URLs in browser or runs help commands defined in the package's `help` attribute. |
| depends.rs | `rez depends` -- Reverse dependency lookup. Given a package name, finds all packages that list it as a dependency. |
| diff.rs | `rez diff` -- Compare two package versions side by side. Shows differences in dependencies, commands, variants, and metadata. |
| plugins.rs | `rez plugins` -- List available package repository plugins and their configuration. |

## Architecture

All query commands follow the same pattern:

1. Parse search paths from args or fall back to `CONFIG.packages_path`
2. Use `repository::package::discover` or `repository::package::search` to find packages
3. Format and print results to stdout

The `search` command uses `PackageSearcher` which supports multiple match modes
(exact, starts-with, contains, regex) and can aggregate results across multiple
repository paths.

The `depends` command performs a full scan of all packages to find reverse
dependencies, using `get_reverse_dependencies()` from the search module.

## Key Types Used

- `repository::package::search::PackageSearcher` -- configurable package search engine
- `repository::package::search::ResourceSearchResult` -- individual search result
- `repository::package::discover::iter_packages` -- iterate packages by name
- `repository::package::discover::iter_package_families` -- list all package names
- `repository::repository::PackageInfo` -- lightweight package metadata

## Usage from main.rs

```rust
Commands::Search(ref args) => cli::query::search::run(args),
Commands::View(ref args) => cli::query::view::run(args),
Commands::HelpPkg(ref args) => cli::query::help_cmd::run(args),
// ...
```
