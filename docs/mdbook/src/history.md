# History of Rez

## The Problem

In the visual effects and animation industry, a single shot or sequence might require Maya 2024, Houdini 20, Nuke 15, Arnold 7, a studio's proprietary lighting tool at version 3.8, Python 3.10, NumPy 1.24, PySide 6.5, and dozens of in-house plugins -- each with their own version constraints and environment requirements. Different departments (lighting, compositing, rigging) need different combinations. Different shows may be locked to different versions. Artists switch between shows multiple times a day.

Before rez, studios handled this with a patchwork of shell scripts, wrapper launchers, and `module` systems borrowed from HPC. These approaches were fragile, hard to debug, and nearly impossible to audit. When an artist reported "Maya crashes on show X but not show Y," tracking the problem to a mismatched library version could take days.

## Origins

Allan Johns (GitHub: nerdvegas) created rez around 2012 while working in the VFX industry. The core insight was simple but powerful: treat software environment configuration as a constraint satisfaction problem. Each package declares what it needs (`requires`) and what it provides (environment variables, tools, library paths). A solver finds a set of package versions that satisfies all constraints simultaneously. If no solution exists, the solver explains exactly which requirements conflict.

The first public release appeared on GitHub and quickly attracted attention from pipeline technical directors at VFX studios worldwide.

## Key Innovations

### Package Definitions as Code

Rez introduced `package.py` -- a Python file that defines package metadata and commands. Because it is executable Python, package authors can use logic:

```python
name = "maya_plugin"
version = "2.1.0"

requires = [
    "maya-2024+",
    "python-3.10+<3.12",
]

def commands():
    env.MAYA_PLUG_IN_PATH.prepend("{root}/plug-ins")
    env.PYTHONPATH.prepend("{root}/python")
    if resolve.maya.version.major == "2024":
        env.MY_COMPAT_FLAG.set("new")
```

This blurred the line between configuration and logic in a productive way. Static formats (YAML, TOML) are also supported for simpler packages.

### Rex: A Domain-Specific Language

The `commands()` function uses "rex" -- a tiny DSL for environment manipulation. Rex provides proxy objects (`env`, `resolve`, `this`, `system`) that generate shell-specific code. The same `package.py` produces correct bash, csh, cmd.exe, or PowerShell output depending on the target shell.

### Resolved Contexts

When rez solves a dependency graph, it produces a "resolved context" that can be serialized to a `.rxt` file. This file captures the exact set of packages, their versions, and the resulting environment. Contexts can be saved, loaded, diffed, and shared. This makes environments reproducible and debuggable.

### Package Repositories

Packages live in "repositories" -- directory trees with a standard layout:

```
/packages/
  maya/
    2024.0/
      package.py
      plug-ins/
    2024.1/
      package.py
      plug-ins/
  python/
    3.10.12/
      package.py
    3.11.8/
      package.py
```

Multiple repositories can be configured (local dev packages, shared studio packages, release packages) with priority ordering.

## Adoption

By 2018, rez had become the de facto standard for environment management in the VFX industry. Major studios adopted it:

- **Weta Digital** (now Weta FX) -- one of the earliest large-scale adopters
- **Industrial Light & Magic (ILM)** -- integrated into their pipeline infrastructure
- **MPC (Moving Picture Company)** -- used across global offices
- **Framestore** -- adopted for show-level environment management
- **DNEG** -- integrated with their build and deployment systems
- **Animal Logic** -- early adopter and contributor
- **Sony Pictures Imageworks** -- adopted for cross-show environment management

The project moved under the **Academy Software Foundation (ASWF)** umbrella, joining OpenEXR, OpenColorIO, OpenTimelineIO, and other foundational VFX open-source projects. This gave it governance structure, CI infrastructure, and increased visibility.

## Timeline

| Year | Event |
|---|---|
| ~2012 | Initial development by Allan Johns |
| 2013-2015 | Early adoption at VFX studios |
| 2016-2017 | Growing community, plugin system, shell support expansion |
| 2018 | Industry standard status, ASWF consideration |
| 2019-2020 | ASWF incubation, rez 2.x releases |
| 2021-2023 | Continued development, Windows support improvements |
| 2024 | Python 2 support dropped, focus on Python 3.9+ |
| 2025-2026 | rez-rs development: Rust port for single-binary deployment |

## The Path to rez-rs

Despite its success, the Python implementation has inherent limitations (discussed in [Motivation](motivation.md)). The rez-rs project began as an effort to preserve the rez model -- packages, versions, rex commands, resolved contexts, repository structure -- while eliminating the deployment and performance overhead of the Python runtime. Its long-term goal is broad interoperability with Python Rez; the current implementation is not yet a drop-in replacement, as detailed in the compatibility report (historical snapshot in old.rez-rs).
