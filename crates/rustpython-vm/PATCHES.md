# RustPython VM 0.6.0 patch

Released RustPython 0.6.0 source, revision `b2626c56dedfbd7d6f516e53f2fc392a514a0b31`, was imported from checksum-verified crates.io archives on 2026-10-07. Root [Cargo.toml](../../Cargo.toml) pins RustPython to `=0.6.0` and applies repository-local dependencies without modifying the registry cache. [Plan30](../../docs/plans/plan30.md#current-status) records migration validation. Earlier 0.5 acceptance describes the old executable and is not acceptance of this update.

The released VM now supplies the former local regex `findall` typed-empty and indexed ASCII/Unicode/WTF-8 driver corrections. [The embedded differential corpus](../../tests/sre_compatibility.rs) remains the regression boundary; source inclusion alone is not runtime acceptance.

The remaining local implementation change is [build.rs](build.rs): Cargo watches the actual packaged `Lib` directory recursively for frozen core/builtin modules, including nested packages. It does not watch the obsolete upstream-checkout `../../Lib/importlib/_bootstrap.py` path. The 0.6 manifest declares Rust 1.95; migration uses Rust 1.99. `stdlib` and `freeze-stdlib` stay enabled together in python-runtime.

The manifest pins Ruff AST/parser/text-size to `=0.16.5`, matching the release lockfile. The initial migration resolved 0.16.10 through upstream caret requirements and failed with ten codegen AST errors. This is a dependency compatibility repair, not a codegen fork. All five Ruff crates are locked to 0.16.5.


The SRE engine now uses an exact `=0.6.0` registry requirement. Its former local copy had no implementation changes and was removed; prefix regressions moved to [the root test suite](../../tests/sre_prefix.rs). See [the replacement and validation](../../docs/mdbook/src/rustpython-vendoring.md).
