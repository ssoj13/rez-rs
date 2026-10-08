# RustPython SRE engine 0.6.0

Released RustPython 0.6.0 source, revision `b2626c56dedfbd7d6f516e53f2fc392a514a0b31`, was imported from checksum-verified crates.io archives on 2026-10-07. Root [Cargo.toml](https://github.com/ssoj13/rez-rs/blob/main/Cargo.toml) pins RustPython to `=0.6.0` and retains the patched runtime owners without modifying the registry cache. [Plan30](https://github.com/ssoj13/rez-rs/blob/main/docs/plans/plan30.md#current-status) records migration validation. Earlier 0.5 acceptance describes the old executable and is not acceptance of this update.

The released engine and string drivers contain the former literal-prefix/capture and coherent character-position corrections. No local engine implementation patch remains. The independent [prefix regression tests](https://github.com/ssoj13/rez-rs/blob/main/tests/sre_prefix.rs) now live in the root test suite; the workspace's [embedded corpus](https://github.com/ssoj13/rez-rs/blob/main/tests/sre_compatibility.rs) verifies the actual CLI across seven APIs and 1,566 reference cases.

The publication follow-up replaces this local source copy with its crates.io 0.6.0 dependency. The production files were byte-identical to the release; the only manifest addition was the retained prefix test target. Current validation is recorded in [RustPython vendoring](rustpython-vendoring.md).
