# RustPython standard library 0.6.0

Released RustPython 0.6.0 source, revision `b2626c56dedfbd7d6f516e53f2fc392a514a0b31`, was imported from checksum-verified crates.io archives on 2026-10-07. Root [Cargo.toml](https://github.com/ssoj13/rez-rs/blob/main/Cargo.toml) pins RustPython to `=0.6.0` and retains the patched runtime owners without modifying the registry cache. [Plan30](https://github.com/ssoj13/rez-rs/blob/main/docs/plans/plan30.md#current-status) records migration validation. Earlier 0.5 acceptance describes the old executable and is not acceptance of this update.

The released source replaces the former local SSL framing implementation with the shared host_env `RecordCursor`. No local stdlib implementation change remains. [TLS tests](https://github.com/ssoj13/rez-rs/blob/main/tests/tls_record_framing.rs) exercise that production cursor directly and use verified loopback TLS, forced short receives, timeouts, MemoryBIO, and strict certificate fixtures.

The remaining certificate/progress guards moved to [host_env](https://github.com/ssoj13/rez-rs/blob/main/crates/rustpython-host_env/PATCHES.md). The buffered zlib correction moved to [common](https://github.com/ssoj13/rez-rs/blob/main/crates/rustpython-common/PATCHES.md). Keep those shared implementations rather than restoring 0.5 adapters. Frozen stdlib remains mandatory for executable relocation.

The main lockfile also unifies pymath’s broadly constrained Malachite dependency with RustPython’s malachite-bigint 0.12.0. Retaining the previous 0.9.2 lock entry produced 17 incompatible BigInt/BigUint errors in math.rs; targeted lockfile update fixes the type identity without math source changes.

The publication follow-up replaces this local source copy with its crates.io 0.6.0 dependency. The production files were byte-identical to the release; the manifest was also unchanged. Current validation is recorded in [RustPython vendoring](rustpython-vendoring.md).
