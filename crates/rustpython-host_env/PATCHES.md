# RustPython Host Environment 0.6.0 TLS patch

Released RustPython 0.6.0 source, revision `b2626c56dedfbd7d6f516e53f2fc392a514a0b31`, was imported from checksum-verified crates.io archives on 2026-10-07. Root [Cargo.toml](../../Cargo.toml) pins RustPython to `=0.6.0` and applies repository-local dependencies without modifying the registry cache. [Plan30](../../docs/plans/plan30.md#current-status) records migration validation. Earlier 0.5 acceptance describes the old executable and is not acceptance of this update.

This new 0.6 crate owns OS/TLS functionality previously implemented in VM/stdlib. Local changes are:

- [ssl/verify.rs](src/ssl/verify.rs): a self-issued certificate is self-signed only if its signature verifies against its own public key. Apply the same strict AKI rule to leaf, intermediates, and supplied roots; only actual self-signed certificates may omit AKI. The manifest enables x509-parser's `verify` feature for that check.
- [ssl/connection.rs](src/ssl/connection.rs): reject a second zero-byte TLS input read after processing packets instead of silently dropping remaining ciphertext.

The released `RecordCursor` retains partial headers/body boundaries and is used by stdlib. [TLS regressions](../../tests/tls_record_framing.rs) verify both cursor fragmentation and actual native TLS consumers. The patch must not disable certificate verification or introduce a second transport.

On Rust 1.99, four release-only `expect(unused_variables)` annotations in [posix_windows.rs](src/posix_windows.rs) are unfulfilled because the parameters are already referenced by debug assertions. Remove those obsolete expectations without adding lint allowances or changing rename/replace behavior.
