# version

Version system for rez packages. Provides parsing, comparison, and range operations
for version strings like "1.2.3", "1.0.alpha", and range expressions like
"1.2+<2.0", ">=1.0,<3.0", "==1.2.3". Ported from Python rez `_version.py`.

This crate depends on `foundation` for shared errors. See the [workspace architecture](../../../docs/mdbook/src/architecture-crates.md) for its consumers. Files below are relative to `src/`.

## Files

| File | Description |
|------|-------------|
| lib.rs | Module declarations and public re-exports: `AlphanumericToken`, `SubToken`, `Version`, `LowerBound`, `UpperBound`, `Bound`, `VersionRange`, `ContainmentMode`, `VersionedObject`, `Requirement`, `RequirementList`. |
| token.rs | Lowest-level building block. `SubToken` (numeric or alpha string segment) and `AlphanumericToken` (sequence of subtokens like "1alpha2"). Ordering: alpha < numeric; numerics compared by value then string. `SubToken` supports `Ord`, `Hash`, `Display`. |
| version.rs | `Version` struct. Parses "1.2.3" into a vector of `AlphanumericToken` separated by `.` or `-`. Special values: `Version::empty()` (smallest), `Version::inf()` (largest/infinity). Supports `Ord`, `Hash`, `Display`, and index access via `get(idx)`. |
| bound.rs | Version range endpoints. `LowerBound` (version + inclusive flag) and `UpperBound` (version + inclusive flag). `Bound` combines lower + upper into a contiguous interval. `Bound::any()` for the universal range. `contains_version()` for point-in-range checks. |
| range.rs | `VersionRange` -- union of `Bound` intervals. Parses complex range expressions: `1.2+` (>=1.2), `<2.0`, `1.0..2.0` (inclusive range), `>=1.0,<2.0` (comma-separated intersection). Supports set operations via operator overloading: `&` (intersection), `|` (union), `-` (difference), `!` (complement). `ContainmentMode` enum. `is_any()`, `is_exact()`, `issuperset()`, `intersects()`, `contains_version()`. |
| requirement.rs | `VersionedObject` -- a named object with a version (e.g. "foo-1.2.3"). `Requirement` -- a package requirement with optional version range and conflict/weak markers (e.g. "foo-1.2+<2.0", "!bar", "~baz"). `RequirementList` -- collection of merged requirements with conflict detection. |

## Architecture

The type hierarchy builds from bottom to top:

```
SubToken          -- "1", "alpha", "03"
    |
AlphanumericToken -- "1alpha2" = [SubToken("1"), SubToken("alpha"), SubToken("2")]
    |
Version           -- "1.2.3" = [Token("1"), Token("2"), Token("3")], seps=['.', '.']
    |
    ├── LowerBound / UpperBound  -- endpoints with inclusive/exclusive
    |       |
    |     Bound                  -- [lower, upper] interval
    |       |
    |   VersionRange             -- union of Bounds, parsed from "1.2+<2.0"
    |       |
    └── Requirement              -- "foo-1.2+<2.0" = name + VersionRange + flags
            |
        RequirementList          -- merged set with conflict detection
```

Version comparison follows rez's Python semantics exactly:

- Empty version < all regular versions
- Infinity > all regular versions
- Tokens compared lexicographically: alpha subtokens < numeric subtokens
- Numeric subtokens compared by value, then by string (for leading zeros)

Separators (`.`, `-`) are preserved for display but do not affect comparison.

## Key Types

| Type | Description |
|------|-------------|
| `Version` | Parsed version with `new(&str)`, `empty()`, `inf()`, `get(idx)`. |
| `VersionRange` | Range expression with set algebra operators `& | - !`. |
| `Requirement` | Named package requirement: `new(&str)`, `name()`, `range()`, `is_conflict()`, `is_weak()`. |
| `RequirementList` | Merged requirement set: `new(Vec<Requirement>)`, `names()`, `get(&str)`, `conflicts()`. |
| `VersionedObject` | Concrete versioned name: `new(&str)`, `name()`, `version()`, `as_exact_requirement()`. |
| `Bound` | Contiguous version interval: `any()`, `contains_version()`, `intersects()`. |

## Usage

```rust
use version::{Version, VersionRange, Requirement, RequirementList};

let v = Version::new("1.2.3")?;
let range = VersionRange::new("1.0+<2.0")?;
assert!(range.contains_version(&v));

let req = Requirement::new("maya-2024+<2025")?;
assert_eq!(req.name(), "maya");

let reqs = RequirementList::new(vec![
    Requirement::new("foo-1.0+")?,
    Requirement::new("foo-1.2+<2.0")?,  // merged with above
])?;
```
