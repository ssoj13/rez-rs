# RustPython 0.6.0 native CLI patch

Released RustPython 0.6.0 source, revision `b2626c56dedfbd7d6f516e53f2fc392a514a0b31`, was imported from checksum-verified crates.io archives on 2026-10-07. Root [Cargo.toml](../../Cargo.toml) pins RustPython to `=0.6.0` and applies repository-local dependencies without modifying the registry cache. [Plan30](../../docs/plans/plan30.md#current-status) records migration validation. Earlier 0.5 acceptance describes the old executable and is not acceptance of this update.

The local changes in [lib.rs](src/lib.rs), [settings.rs](src/settings.rs), and [shell.rs](src/shell.rs) preserve whole-program redirected stdin, empty no-selector argv[0], main/file/cache metadata, stream encoding, exception/exit/atexit behavior, and explicit `-i` inspection. `PYTHONINSPECT` alone does not turn a redirected stream into a REPL. Terminal-only history is optional and cannot change the program's exit status. The 0.6 pyrepl fallback warning remains on terminal input.

The shared upstream parser and public runner remain the only Python CLI path. [Re-entry tests](../../tests/python_reentry.rs) and [relocation tests](../../tests/python_portability.rs) verify this contract. Remove these local changes only after the released CLI passes the same consumers.

The independent parser dev-dependency is also pinned to `=0.16.5`; see [VM dependency compatibility](../rustpython-vm/PATCHES.md).


Registry stdlib now uses an exact `=0.6.0` requirement in normal and development dependencies. The former local stdlib copy had no implementation changes and was removed; see [the replacement and validation](../../docs/mdbook/src/rustpython-vendoring.md).

The upstream README's links to files outside the published crate now point to the recorded upstream revision rather than missing local paths. This is a documentation-only rebase.
