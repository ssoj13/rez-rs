# NoOp Build System Example

Package with **no build**: used when there are no known build system files.

## Detection

Rez selects NoOp when:
- No build system is detected (no CMakeLists.txt, Makefile, setup.py, etc.)
- Or explicitly set `build_system = "noop"`

## Build

```bash
rez build --install
```

Steps: none (build succeeds immediately, install copies package definition).

## Structure

- `package.py` — metadata and commands only, no build files
