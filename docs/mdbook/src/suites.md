# Suites

A **suite** is a collection of resolved contexts with a single `bin/` directory in PATH. You add one path and get access to tools from all contexts — Maya, Houdini, Nuke, Python, etc. — without entering each environment separately.

## When to use a suite

| Scenario | Use |
|----------|-----|
| Maya + Redshift in one session | `rez env maya-2024 redshift` — one context |
| Houdini + Redshift in one session | `rez env houdini-20 redshift` — one context |
| Run Maya, Houdini, Nuke from one PATH | Suite with separate contexts for each |
| Artists run `maya` or `houdini` without rez knowledge | Suite |

**One context** = one resolve. Maya+Redshift and Houdini+Redshift are separate contexts because they need different DCCs. A suite unifies them: one `bin/` with `maya`, `houdini`, etc.

## Quick start

### 1. Create a suite

```bash
rez suite ~/suites/studio --create
```

Creates `~/suites/studio/` with `suite.yaml`, `bin/`, `contexts/`.

### 2. Resolve and save contexts

Each context is a resolved request, saved as `.rxt`:

```bash
# Maya + Redshift
rez env maya-2024 redshift -o /tmp/maya.rxt

# Houdini + Redshift
rez env houdini-20 redshift -o /tmp/houdini.rxt

# Python (for scripts)
rez env python-3.11 -o /tmp/python.rxt
```

### 3. Add contexts to the suite

```bash
rez suite ~/suites/studio --add /tmp/maya.rxt --context maya
rez suite ~/suites/studio --add /tmp/houdini.rxt --context houdini
rez suite ~/suites/studio --add /tmp/python.rxt --context python
```

Each `--add` copies the `.rxt` into `contexts/<name>.rxt` and generates wrapper scripts in `bin/` for all tools declared in the request packages.

### 4. Add suite bin to PATH

**Linux/macOS:**
```bash
export PATH="$HOME/suites/studio/bin:$PATH"
```

**Windows PowerShell:**
```powershell
$env:PATH = "$env:USERPROFILE\suites\studio\bin;$env:PATH"
```

### 5. Run tools

```bash
maya        # launches Maya with Redshift
houdini     # launches Houdini with Redshift
python      # Python 3.11
```

No `rez env` needed — the wrapper loads the right context and runs the tool.

## Tools and package.py

Tools exposed by a suite come from the `tools` attribute of packages in the **request** (not dependencies).

Example Maya package:
```python
# maya/2024/package.py
name = "maya"
version = "2024"
tools = ["maya", "mayapy", "Render", "fcheck"]

def commands():
    env.PATH.prepend("{root}/bin")
```

Redshift may add `redshift` or integrate into Maya's commands. If both provide `python`, you'll have a conflict (see below).

## Tool conflicts

When several contexts provide the same tool (e.g. `python` from Maya and from a standalone Python package), the suite resolves by **priority**: the last added or bumped context wins.

### Prefix / suffix

Give all tools from a context a prefix or suffix:

```bash
rez suite ~/suites/studio --prefix maya_ --context maya
# Tools: maya_maya, maya_mayapy, maya_python, ...

rez suite ~/suites/studio --prefix hou_ --context houdini
# Tools: hou_houdini, hou_hython, ...
```

Then no conflict — `maya_python` and `hou_hython` are distinct. Or keep `maya` and `houdini` without prefix, and hide overlapping tools.

### Hide tools

```bash
rez suite ~/suites/studio --hide mayapy --context maya
```

`mayapy` is no longer in `bin/` — useful when you prefer the suite's standalone `python`.

### Alias

```bash
rez suite ~/suites/studio --alias Render maya_render --context maya
```

Exposes `maya_render` as an alias for Maya's `Render` tool.

### Bump (raise priority)

```bash
rez suite ~/suites/studio --bump maya
```

Maya's context gets higher priority; its tools override others in case of conflicts.

## Common suite commands

```bash
rez suite ~/suites/studio --tools        # list exposed tools
rez suite ~/suites/studio --which maya  # path to maya wrapper
rez suite ~/suites/studio -l             # list visible suites on PATH (if bin in PATH)
rez suite ~/suites/studio --validate     # check suite integrity

# Modify
rez suite ~/suites/studio --remove houdini
rez suite ~/suites/studio --add /tmp/nuke.rxt --context nuke
```

## Updating a context

When you upgrade Maya or Redshift:

```bash
rez env maya-2025 redshift -o /tmp/maya.rxt
rez suite ~/suites/studio --add /tmp/maya.rxt --context maya
```

Adding with the same `--context` overwrites the existing context and regenerates wrappers.

## Under the hood

- **suite.yaml** — metadata: context names, paths, prefixes, hidden tools, aliases.
- **contexts/*.rxt** — serialized resolved contexts (packages, env, tools).
- **`bin/<tool>`** — wrapper script; runs `rez forward <self> -- <args>`. The wrapper embeds YAML: context path, tool name. `rez forward` loads the context and executes the tool in that environment.

## Example: studio setup

```bash
# Create studio suite
rez suite /studio/suites/tools --create

# Maya 2024 + Redshift + internal tools
rez env maya-2024 redshift mycompany_maya_tools -o /tmp/maya.rxt
rez suite /studio/suites/tools --add /tmp/maya.rxt --context maya

# Houdini 20 + Redshift
rez env houdini-20 redshift -o /tmp/hou.rxt
rez suite /studio/suites/tools --add /tmp/hou.rxt --context houdini

# Prefix to avoid conflicts (both have Python)
rez suite /studio/suites/tools --prefix maya_ --context maya
rez suite /studio/suites/tools --prefix hou_ --context houdini

# Add to system PATH (e.g. in /etc/profile.d or Windows system env)
export PATH="/studio/suites/tools/bin:$PATH"
```

Artists open a terminal, run `maya` or `houdini` — no rez commands needed.
