# RustPython Common 0.6.0 compression patch

Released RustPython 0.6.0 source, revision `b2626c56dedfbd7d6f516e53f2fc392a514a0b31`, was imported from checksum-verified crates.io archives on 2026-10-07. Root [Cargo.toml](../../Cargo.toml) pins RustPython to `=0.6.0` and applies repository-local dependencies without modifying the registry cache. [Plan30](../../docs/plans/plan30.md#current-status) records migration validation. Earlier 0.5 acceptance describes the old executable and is not acceptance of this update.

The released [StrData](src/str.rs) and [Wtf8Index](src/wtf8_index.rs) contain the former coordinated Unicode/string-index backport. Those files now use the released implementation.

The remaining local change is [compression/zlib.rs](src/compression/zlib.rs), where 0.6 moved the decompression backend. Refresh input after every consumed chunk, drain buffered output with empty input until EOF/no progress/output limit, enforce the caller's output limit, retry requested dictionaries, and retain `needs_input = false` when the output limit may leave buffered output. Upstream already records the actual stream end during `flush`; that old local change is unnecessary.

[Compression regressions](../../tests/python_compression.rs) exercise zlib, dictionaries, truncated streams, bounded bz2/lzma output, and ZIP CRC integrity. Native cached-archive consumers retain their separate Plan30 gates.
