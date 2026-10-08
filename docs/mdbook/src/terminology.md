# Terminology

This glossary defines terms used throughout rez-rs documentation, CLI output, and source code.

---

**Action**
A single environment manipulation command in the rex DSL. Actions include `Setenv`, `Unsetenv`, `Prependenv`, `Appendenv`, `Alias`, `Source`, `Command`, `Info`, `Error`, `Comment`, `Shebang`, and `Stop`. Actions are shell-agnostic; they are rendered to shell-specific code by an `ActionInterpreter`.

---

**Bind**
The process of detecting system-installed software and creating corresponding rez package definitions. For example, `rez bind python` detects the installed Python version and creates a `python` package in the local repository. `rez bind --all` scans for and binds everything found on the system (platform/arch/os, Python stack, tools, DCC apps) in dependency order.

---

**Build Requires**
Dependencies needed only at build time, not at runtime. Specified in the `build_requires` field of `package.py`. These packages are included in the resolve when building but not when a consumer resolves the built package.

---

**Bundle**
A self-contained copy of a resolved context including all package payloads. Created by `rez bundle`. Bundles can be transferred to offline machines (render nodes, air-gapped systems) where the original package repositories are not accessible.

---

**Cachable**
A package attribute indicating whether the package can be copied to a local cache for faster access. Controlled by the `cachable` field in `package.py` or the `default_cachable` config setting.

---

**Commands**
A function or string in `package.py` that defines how the package configures the shell environment. Written in the rex DSL. Executed when the package is part of a resolved context.

---

**Conflict Requirement**
A requirement prefixed with `!` that declares incompatibility. `!foo` means the package `foo` must NOT be present in the resolve. Used to express that two packages cannot coexist.

---

**Context**
See Resolved Context.

---

**@early Decorator**
A decorator applied to a field function in `package.py` that causes it to be evaluated at package load time. The function's return value replaces the field. Used for dynamic dependencies that depend on the environment at the time the package is loaded.

---

**Ephemeral Package**
A virtual package prefixed with `_.` that exists only during a resolve. Not installed anywhere. Used to pass configuration flags to resolves (e.g., `rez env myapp _.debug=1`). Packages can check for ephemerals in their `commands()` using `if "_.debug" in request`.

---

**Extraction**
A phase in the solver algorithm where common dependencies shared by ALL variants in a scope are identified and promoted to their own scopes. For example, if every version of package A requires package B, then B is "extracted" as a dependency.

---

**Implicit Package**
A requirement automatically added to every resolve, configured via `implicit_packages` in rezconfig. Typically constrains platform, architecture, and OS so that resolves only consider packages compatible with the current system.

---

**include()**
A function available in `package.py` that imports shared code from `_include/` directories in the packages_path. Allows reusing common functions and definitions across multiple packages.

---

**Intersection**
A phase in the solver where extracted requirements are combined with existing scopes, narrowing the set of acceptable versions. If scope A requires `python-3.10+` and the python scope has 3.8-3.11, intersection narrows it to 3.10-3.11.

---

**@late Decorator**
A decorator applied to a field function in `package.py` that causes it to be evaluated at resolve time rather than load time. This is the default for `commands`, `pre_commands`, and `post_commands`.

---

**Package**
A named, versioned piece of software with metadata, dependencies, and environment commands. Defined by a `package.py`, `package.yaml`, or `package.toml` file. The fundamental unit in rez.

---

**Package Family**
The collection of all versions of a package with the same name. For example, the "python" family contains versions 3.9.18, 3.10.12, 3.11.8, etc.

---

**Package Filter**
A rule that excludes certain packages from resolves. Configured via `package_filter` in rezconfig. Used to blacklist broken versions or restrict available packages.

---

**Package Order**
A rule that determines the preferred order of package versions during resolution. By default, newer versions are preferred. Custom orderers can implement strategies like timestamp-based ordering. Configured via `package_orderers` in rezconfig.

---

**Package Provider**
A trait (`PackageProvider`) that supplies package data to the solver. Implementations include `FsRepo` (reads from disk) and `MemoryPackageRepository` (in-memory, for testing).

---

**Package Repository**
See Repository.

---

**packages_path**
A list of filesystem paths where rez looks for packages, searched in priority order. The first repository containing a matching package wins. Configured in `rezconfig.py`.

---

**Patch**
Modifying an existing resolved context by adding or changing requests. `rez env --patch mylib-3.0+` takes the current context and adjusts it. The `PatchLock` enum controls how tightly the existing resolve is preserved.

---

**Reduction**
A phase in the solver where variants are removed because their dependencies conflict with other scopes. A `Reduction` record captures which variant was removed, which dependency it had, and which existing request it conflicted with.

---

**Relocatable**
A package attribute indicating whether the package can be moved to a different filesystem location without breaking. Non-relocatable packages may contain hardcoded paths. Controlled by the `relocatable` field in `package.py` or the `default_relocatable` config setting.

---

**Repository**
A directory tree containing rez packages. Standard layout: `<repo>/<name>/<version>/package.py`. Multiple repositories are configured via `packages_path` with priority ordering. Two implementations exist: `FsRepo` (real files) and `MemoryPackageRepository` (in-memory).

---

**Requirement**
A package dependency specification combining a name and version range. Examples: `"maya-2024+"`, `"python-3.10+<3.12"`, `"!legacy_tool"`, `"~openexr-3+"`. Three types: normal, conflict (`!`), and weak (`~`).

---

**RequirementList**
An ordered collection of Requirements with methods for intersecting, merging, and checking compatibility.

---

**Resolved Context**
The result of a successful dependency resolve. Contains the original request, the resolved package list (exact name-version-variant tuples), the resulting environment changes, and metadata. Can be saved to `.rxt` files and loaded later. Created by `ResolvedContext::resolve()`, consumed by `ResolvedContext::execute()`.

---

**Resolver**
The high-level wrapper around the Solver. Provides builder-pattern configuration, status mapping, and resolved package info extraction. Defined in `resolve/resolver.rs`.

---

**Rex**
Rez EXecution -- the domain-specific language for environment manipulation. Rex provides proxy objects (`env`, `this`, `resolve`, `system`) that generate shell-specific code. Rex commands are Python code executed by RustPython.

---

**RexExecutor**
The high-level component that combines the ActionManager (env state tracking), ActionInterpreter (shell rendering), and variant bindings to execute rex commands. Defined in `shell/rex.rs`.

---

**RXT (.rxt file)**
A JSON file containing a serialized Resolved Context. Stores the complete resolve state: request, packages, environment, metadata. Used for reproducibility, sharing, and debugging.

---

**Scope**
A solver data structure representing the set of candidate versions for a package. Scopes are narrowed through extraction, intersection, and reduction until each scope contains exactly one version (solved) or is empty (failed).

---

**Shell**
A target shell type for environment generation. rez-rs supports: bash, sh, csh, tcsh, cmd, powershell, gitbash. Each shell has its own `ActionInterpreter` implementation that renders rex Actions into the correct shell syntax.

---

**Solver**
The dependency resolution engine. Uses a phase-based backtracking algorithm: extract common dependencies, intersect with scopes, reduce conflicts, split if stuck, backtrack if failed. Defined in `resolve/solver.rs`.

---

**Suite**
A collection of resolved contexts with unified tool aliases. Allows multiple environments to coexist with tool wrappers that activate the correct context when invoked.

---

**Variant**
A platform-specific or configuration-specific build of a package. Each variant has additional requirements (e.g., `["python-3.10"]`) and is installed into its own subdirectory. The resolver picks the variant whose requirements are compatible with the rest of the resolve.

---

**Version**
A dot-separated string of numeric and alphanumeric tokens (e.g., `"1.2.3"`, `"2024.0"`, `"3.10.12"`). Compared token-by-token. The empty version represents an unversioned package.

---

**VersionRange**
A constraint on acceptable versions. Supports exact versions (`1.2.3`), lower bounds (`1.2+`), upper bounds (`<2.0`), ranges (`1.2+<2.0`), and unions (`1.2|2.0+`).

---

**VersionedObject**
An object with a name and version, used as a common type in the solver for representing packages, requirements, and reductions.

---

**Weak Requirement**
A requirement prefixed with `~`. Does not pull in the package, but constrains its version if it is already present. `"~openexr-3+"` means: if openexr is in the resolve, it must be version 3 or later.
