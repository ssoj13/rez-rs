# Getting Started

This chapter walks through setting up rez-rs from scratch: creating a package repository, binding system software, writing your first package, and resolving environments.

## Step 1: Create a Package Repository

A package repository is a directory tree where rez packages live. Create one:

```bash
mkdir -p ~/packages
```

On Windows:

```powershell
mkdir $env:USERPROFILE\packages
```

The standard layout inside a repository is:

```
~/packages/
  <package_name>/
    <version>/
      package.py      (or package.yaml / package.toml)
      ...             (package payload: binaries, libraries, scripts)
```

## Step 2: Create rezconfig.py

rez-rs reads its configuration from `rezconfig.py`. Create it in the standard location:

```bash
mkdir -p ~/.rez
```

Create `~/.rez/rezconfig.py`:

```python
# Where rez looks for packages, in priority order (first match wins)
packages_path = [
    "~/packages",           # Local development packages
    "/studio/packages",     # Shared studio packages (if applicable)
]

# Where 'rez release' deploys packages
release_packages_path = "~/packages"

# Where 'rez build' puts local installs
local_packages_path = "~/packages"

# Implicit packages added to every resolve
implicit_packages = [
    "~platform=={system.platform}",
    "~arch=={system.arch}",
    "~os=={system.os}",
]

# Default shell for rez env
default_shell = ""   # Empty = auto-detect
```

The configuration loading order is:

1. Built-in defaults
2. Site `rezconfig.py` (if found in standard locations)
3. User `~/.rez/rezconfig.py`
4. `REZ_CONFIG_FILE` environment variable
5. Individual `REZ_*` environment variables

## Step 3: Bind System Software

Before resolving environments, rez needs to know about the software on your system. The `bind` command detects installed software and creates package definitions:

```bash
rez bind --quickstart
```

This binds 5 core packages:

| Package | What it detects |
|---|---|
| `platform` | `windows`, `linux`, or `osx` |
| `arch` | `x86_64`, `aarch64`, etc. |
| `os` | `windows-10`, `ubuntu-22.04`, `osx-14.0`, etc. |
| `python` | System Python (creates venv in package) |
| `rez` | rez-rs itself (copies binary into package) |

To run the available binders against tools found on your system:

```bash
rez bind --all
```

To bind specific packages:

```bash
rez bind platform
rez bind python
rez bind cmake
rez bind maya
rez bind -i ~/packages python   # custom install path
rez bind --all -v                # bind all + verbose (list each package)
```

After binding, your repository will contain:

```
~/packages/
  platform/
    windows/
      package.py
  arch/
    x86_64/
      package.py
  os/
    windows-10.0/
      package.py
  python/
    3.10.12/
      package.py
  ...
```

To see what was bound:

```bash
rez search --paths ~/packages
```

## Step 4: Write Your First Package

Create a simple tool package. Make the directory structure:

```bash
mkdir -p ~/packages/hello/1.0.0
```

Create `~/packages/hello/1.0.0/package.py`:

```python
name = "hello"
version = "1.0.0"
description = "A simple hello world package"

authors = ["Your Name"]

tools = ["hello"]

def commands():
    env.PATH.prepend("{root}/bin")
```

Create the tool itself:

```bash
mkdir -p ~/packages/hello/1.0.0/bin
```

Create `~/packages/hello/1.0.0/bin/hello` (or `hello.bat` on Windows):

**Linux/macOS:**
```bash
#!/bin/bash
echo "Hello from rez! Package root: $REZ_HELLO_ROOT"
```

**Windows (`hello.bat`):**
```batch
@echo off
echo Hello from rez! Package root: %REZ_HELLO_ROOT%
```

Make it executable (Linux/macOS):

```bash
chmod +x ~/packages/hello/1.0.0/bin/hello
```

## Step 5: Resolve an Environment

Now enter an environment with your package:

```bash
rez env hello
```

This will:

1. Find `hello-1.0.0` in your repository
2. Resolve all dependencies (none in this simple case)
3. Execute the `commands()` -- prepending the bin directory to PATH
4. Spawn a new shell with the modified environment

Inside the resolved shell, you can run:

```bash
hello
# Output: Hello from rez! Package root: /home/you/packages/hello/1.0.0
```

Type `exit` to leave the resolved environment.

## Step 6: Search and View Packages

List all available packages:

```bash
rez search
```

Search for a specific package:

```bash
rez search hello
```

View detailed information:

```bash
rez view hello
```

This shows the package's name, version, description, requires, commands, and other metadata.

## Step 7: Create a Package with Dependencies

Create a more realistic package. First, create a library package:

```bash
mkdir -p ~/packages/mylib/2.0.0
```

`~/packages/mylib/2.0.0/package.py`:

```python
name = "mylib"
version = "2.0.0"
description = "A shared library package"

requires = [
    "python-3.9+",
]

def commands():
    env.PYTHONPATH.prepend("{root}/python")
```

Now create a tool that depends on it:

```bash
mkdir -p ~/packages/mytool/1.0.0
```

`~/packages/mytool/1.0.0/package.py`:

```python
name = "mytool"
version = "1.0.0"
description = "A tool that uses mylib"

requires = [
    "mylib-2+",
    "python-3.10+",
]

tools = ["mytool"]

def commands():
    env.PATH.prepend("{root}/bin")
```

Resolve both together:

```bash
rez env mytool
```

The resolver will pull in `mytool-1.0.0`, `mylib-2.0.0`, and `python-3.10.x` (from your bound system Python). All three packages' commands will be executed in order, configuring PATH and PYTHONPATH.

## Step 8: Build an Example Package

Use the checked-in example recipes instead of creating an incomplete build project. Install CMake and a C++ compiler, and configure a writable `local_packages_path` first.

From the rez-rs checkout:

```bash
cd examples/cmake
rez build --install
```

The directory already contains `package.py`, `CMakeLists.txt`, and the C++ source. Successful installation publishes `example_cmake-1.0.0` into the configured local repository. You can then try:

```bash
rez env example_cmake -- example_cmake
```

See [Example Packages](examples.md) for other adapters and their prerequisites. These are workflow examples; platform and toolchain behavior still need validation for your setup.

## Example: Maya Plugin Environment

A realistic VFX studio setup might look like this:

```bash
# Bind system software
rez bind --quickstart

# Enter a Maya development environment
rez env maya-2024 python-3.10 mylib-2+ -- maya
```

The `--` separator passes remaining arguments as a command to run inside the resolved environment instead of spawning an interactive shell. This launches Maya with all environment variables configured correctly.

## Next Steps

- Read [Core Concepts](concepts.md) for a deeper understanding of packages, variants, and resolves
- Read [Configuration](configuration.md) to customize rez-rs behavior
- Read [CLI Commands](commands.md) for the full command reference
- Read [Package Format](package-format.md) for the complete package.py specification
