# Plan 30 — Current Compatibility Work

Updated: 2026-10-08. This is the only retained work plan. Previous plans and audit reports were removed from the new repository; prior Git history is retained separately in `old.rez-rs`. Upstream Rez is an external behavioral reference, not a bundled checkout.

## Current status

The workspace contains ten functional crates under `crates/rez/`; the root package builds the CLI. See [crate architecture](../mdbook/src/architecture-crates.md).

RustPython is pinned to 0.6.0. Stdlib and SRE use crates.io; four active local patches remain. The registry replacement passed a fresh default-feature Windows release build and 27 scoped Rust tests across six Python/SRE/TLS targets, plus 23 Python helper tests. The earlier migration campaign recorded 1,496 unique executed Rust tests, strict workspace Clippy, and 14 passing doc-tests; those historical gates were not repeated for the registry replacement. See [runtime ownership and scoped evidence](../mdbook/src/rustpython-vendoring.md).

The book lives in `docs/mdbook`; generated HTML is excluded from source packages. Current source and installer changes require updated source-closure verification. These scopes do not establish full Rez parity, GUI runtime, Linux/macOS acceptance, or current host activation.

## GitHub CI and release work

- [x] Add push/PR/manual checks and a default-feature Windows x86_64 build through the canonical dist staging command. Version-matching `v*` tags publish the verified `rez.exe`, checksum, and receipt.
- [x] Validate the workflow with actionlint 1.7.12 and pass 25 Python helper tests, including credential-host scoping and cleanup after a failed fetch.
- [x] Confirm a relocated Windows executable generates the commented default config and runs frozen Python with no host Python on PATH. Quickstart creates platform/arch/os/rez definitions; the unsupported legacy rezgui entry was removed from its requested list.
- [x] Verify the staged source archive (739 members, exact bytes and CRCs), canonical helper equality, frozen runtime, no-Python quickstart, bound Rez execution, and installed CPython execution. The native Rez binding no longer declares an external Python dependency.
- [x] Before adding the CPython bridge, the final binder source passed one full release workspace campaign: 1,507 test/doc-test executions, zero failures and seven ignored scenarios. Strict release all-target workspace Clippy passed separately. These results do not establish acceptance of later Python API changes.
- [x] Verify the new `rez-python-api` bridge and extracted `rez.rs` distribution: 14 acceptance tests passed on each of CPython 3.13.11 and 3.10.18 against the same Windows abi3 ZIP; all 14 archive members matched canonical bytes and CRCs. Strict release all-target workspace Clippy passed after the bridge changes.
- [x] After adding the CPython bridge, one full release workspace campaign passed: 1,507 test/doc-test executions, zero failures, seven ignored scenarios, across 46 targets. The interrupted compile attempt is not a test receipt.
- [x] Configure read-only deploy keys for the four private GUI repositories and their corresponding Actions Secrets. Each key passed a fresh SSH read check; private key scratch files were removed. The CI fetch helper has 30 passing Python helper tests, including credential isolation and cleanup.
- [ ] Accept a fresh hosted GitHub run after the Windows path-authority changes. Run `37734529235` fetched the locked dependencies but failed the earlier Pip Windows 8.3-path regression. Later run `37742726286` passed that Pip test but failed repository copying with mixed root spellings. Both runs remain failed historical receipts; a local Pip repair does not establish repository or full hosted acceptance. Run [`37750577006`](https://github.com/ssoj13/rez-rs/actions/runs/37750577006) at `b3bbda6` passed both earlier path regressions but stopped the normal-path campaign in the PowerShell stdin UTF-8 BOM fixture; the current fixture repair needs a fresh hosted run. Fork PRs run source checks only while the dependency repositories remain private.

## Windows path authority and CI

The shared [path-authority contract](../mdbook/src/architecture-crates.md#path-authority-and-source-identity)
separates lexical keys, configured/canonical/8.3 root relations, generated-component
validation, existing source identity, staging coordinates, and metadata-content
relocation. It preserves configured junction roots while rejecting generated
junctions, parent traversal, and outside destinations.

- [x] Verify the current path-authority implementation and its existing consumers, including repository copy, read-only lookup, SourceMap, and generated-directory checks. The local `b3bbda6` Windows source gate below passed; strict release all-target workspace Clippy passed in 14.919 seconds, all 33 main Python helper tests passed, and formatting, mdbook, and actionlint checks passed. The receipt is `dist/windows-path-gates.json`; artifact and hosted acceptance remain separate.
- [x] Run `python ci/test_workspace.py --require-short-path` locally at `b3bbda6`: both full locked release workspace campaigns passed, with normal temporary paths and a genuine Windows 8.3 `TEMP` alias. Each campaign passed 1,528 test/doc-test executions across 47 targets, zero failures and seven ignored scenarios; the matrix took 571.282 seconds. The total 3,056 executions repeat the same scopes in the two path environments. Missing alias capability fails the gate.
- [ ] Accept the same two-campaign gate in a fresh hosted Windows run; failed runs `37734529235` and `37742726286` retain their separate Pip and mixed-root repository-copy failures.
- [x] Rebuild and verify CLI/Python/source artifacts after the path changes at `b3bbda6`. `bootstrap.py p --force` passed in 77.508 seconds; `ci/verify_dist.py --require-python` passed in 16.068 seconds, checking all 760 source members against canonical bytes and CRCs, installer helper equality, relocated CLI/frozen Python, quickstart, and native CPython consumers. The same fresh Windows abi3 ZIP passed 15 extracted-package API tests on each of CPython 3.13.11 and 3.10.18; formatting passed. The receipt is `dist/windows-path-artifacts.json`. These local checks do not establish hosted CI or external recipe installation. The earlier 1,522-test and 759-source-member receipts below retain their original scope.

## PowerShell pipe encoding and current CI

[Hosted run `37750577006`](https://github.com/ssoj13/rez-rs/actions/runs/37750577006)
at `b3bbda6` passed the earlier Pip and mixed-root repository-copy regressions,
then stopped in the normal-temporary-path PowerShell fixture on a UTF-8 BOM in
native stdin. It did not complete the normal campaign or reach full 8.3-path
acceptance. The failed run does not invalidate the separate local `b3bbda6`
receipts above or establish a passing hosted gate.

The byte-level local reproduction distinguishes PowerShell's `$OutputEncoding`
for text piped to native stdin from `[Console]::OutputEncoding` for console
output. A BOM-enabled `$OutputEncoding` produces the `EF BB BF` prefix even when
console output encoding omits the BOM. The fixture repair sets both encodings
explicitly to UTF-8 without a BOM. A native baseline regression covers both
explicitly selected caller BOM preferences, Unicode input, and preservation of
the caller's `$OutputEncoding` object by rendering. This changes test setup, not the production shell renderer.

- [x] Accept the repaired fixture and native BOM/Unicode/preference-identity baseline in both full local workspace campaigns. Normal and genuine Windows 8.3 temporary paths each passed 1,529 test/doc-test executions across 47 targets, zero failures and seven ignored scenarios, totaling 3,058 repeated executions; the matrix took 763.677 seconds. The raw `EF BB BF` baseline assertion passed on Windows PowerShell and PowerShell 7. Strict release all-target workspace Clippy passed in 7.558 seconds without warnings; all 33 main Python helper tests, formatting, mdbook, and actionlint passed. The receipt is `dist/powershell-encoding-gates.json`; artifact and hosted acceptance remain separate.
- [x] Rebuild and reverify current CLI/Python/source artifacts after the fixture and workflow edits. Packaging passed in 48.871 seconds, and the same fresh Windows abi3 ZIP passed 15 extracted-package API tests on each of CPython 3.13.11 and 3.10.18. The first distribution check correctly detected a documentation/archive byte mismatch and remains a failed attempt; after canonical archive regeneration, `distribution-retry1` passed all 760 source-member byte/CRC checks, installer helper equality, exclusions, relocated CLI/frozen runtime, quickstart with and without host Python, and the bound CPython consumer. The receipt is `dist/powershell-encoding-artifacts.json`. The earlier `b3bbda6` artifact receipt retains its historical scope; hosted acceptance remains pending.
- [ ] Accept a fresh hosted run after the PowerShell fixture and CI cache changes. The prior run above remains a failed receipt.

CI restores and saves its compiled Cargo cache in separate steps. A trusted
non-PR run attempts the cache save after the workspace attempt even when a
runtime test fails, so a cold compile can be reused. Saving a compilation cache
does not establish runtime or release acceptance.

## Shared environment contract

The accepted names are `REZ_SOURCES_PATH`, `REZ_WHEEL_CACHE_PATH`, `REZ_USER_PATH`,
`REZ_OFFLINE`, `REZ_REPO_PATH`, and `REZ_LOG_LEVEL`. PBS recipe overrides use
`REZ_PBS_*`; package versions remain `REZ_BUILD_PROJECT_VERSION`. The obsolete
external installer adapter is removed. Recipes live in the separate private
`rez-rs-packages` repository and still require a compatible external `rez_build`
helper.

- [x] Verify the current source contract across canonical config, generated defaults, Rex/build child environments, publication precedence, and managed acquisition. The Windows release workspace gate passed 1,522 test/doc-test executions across 47 targets, zero failures and seven ignored scenarios; strict all-target workspace Clippy passed. Main Python helpers passed 33 tests; recipe controls passed 34 tests on CPython 3.13 and 3.10. Recipe syntax/just parsing covered 81 Python files and 65 justfiles. Earlier failed compile/cache-path attempts remain separate receipts under dist; this scope does not establish external recipe installation.
- [x] Regenerate release/source artifacts after the contract changes with `bootstrap.py p --force`. `ci/verify_dist.py --require-python` verified 759 source members against canonical bytes and CRCs, installer helper equality, the frozen CLI runtime, and quickstart consumers. `ci/verify_python_api.py` passed 15 extracted-package acceptance tests on each of CPython 3.13.11 and 3.10.18 against the same Windows abi3 ZIP. Formatting passed. These scoped local receipts precede the Windows path-authority rework and do not establish acceptance of those later changes, hosted CI, host activation, or external recipe installation.
- [ ] Accept actual external-helper recipe builds separately. Controlled tests and source inspection do not establish installation of the recipe queue.

See [settings, strict booleans, and acquisition scope](../mdbook/src/configuration.md#shared-source-cache-and-offline-settings). `REZ_OFFLINE` defaults to false and accepts `true`/`false`; it is not a network sandbox.

## Next execution order

- [x] P0: current RustPython 0.6.0 workspace compiler/test/strict-lint gates pass, including moved library tests and explicitly executed native/ignored tests. Preserve the earlier failed campaigns and composite receipts as historical evidence; current single-campaign and native scopes are recorded above.
- [x] P0: regenerate the RustPython 0.6.0 release package and verification report at the final document/source boundary; verify every archive member and canonical installer helper. Wider Bootstrap payload/cache availability remains in A6.
- [ ] P0: deploy that accepted CLI through the existing native activation path and verify primary/aliases, ownership, journal state, isolated Python, and actual CLI consumers. The observed host hash alone is insufficient.
- [ ] P1: implement and verify the missing package preprocessing execution path described in A1; resolve concrete external-CPython API consumers in A3.
- [ ] P1: finish differential lifecycle/Rex/context/cache/copy acceptance at the existing shared boundaries. Do not recreate source-implemented functionality from old absence claims.
- [ ] P2: execute remaining actual ecosystem/platform/GUI consumers and user-launched Bootstrap recipes, preserving all A1–A8 obligations below.

## A1 — Python source lifecycle and package representation

Current owners: [model serialization](../../crates/rez/model/src/serialise.rs), [Package/DeveloperPackage](../../crates/rez/model/src/package/core.rs), [Python runtime](../../crates/rez/python-runtime/src/lib.rs), and [locked repository publication](../../crates/rez/repository/src/repository.rs).

- [x] Source: canonical Python/YAML/TOML loading, configured definition stems, schema validation, unknown attribute retention, early/deferred evaluation, and shared include preparation exist. `source_with_includes` parses function identity through Python AST, supports raw bodies/function invocation, and uses `.rez/include` for installed modules.
- [x] Source: typed native version/requirement bindings, include-module caching, source identity, original Python definition verification/publication, and error propagation have implementations and regression sources. These are not blanket absence findings.
- [ ] Implement global/local package preprocessing in the original namespace with before/after/override order and exception semantics. Current `package_preprocess_function`/`package_preprocess_mode` config declarations and serialization exist, but this review found no package-loader invocation; retaining a `preprocess` field is not executing it.
- [ ] Complete the public SourceCode representation: commands are still `Option<String>` in Package. Reconcile body/function mode, filename/includes, writer/cache epoch, original namespace, and exactly-once execution before claiming the full typed contract.
- [ ] Extend differential fixtures through resolver/build/test/release/tools consumers for variant/context-dependent deferred attributes, `get_objects`, extra objects, field ownership, and source provenance through installed reload.
- [ ] Complete Python/YAML/TOML parse/write/error round trips, including unknown/null/falsey/custom attributes. Existing tests for aliases, include-cache invalidation/retry, early native objects, and late errors should be extended where needed, not duplicated.

## A2 — Contexts, solver, repositories, and services

Current owners: [resolved contexts](../../crates/rez/resolve/src/context.rs), [bundle orchestration](../../crates/rez/resolve/src/bundle_context.rs), [repository providers/provenance](../../crates/rez/repository/src/provider.rs), [copy/move](../../crates/rez/repository/src/package/ops.rs), and [payload cache](../../crates/rez/repository/src/package/cache.rs).

- [x] Source: canonical JSON/YAML context loading, validated ResourceHandles, exact candidate hydration, and marker-relative handle locations on save/load exist. Pre-4.0 migration has an explicit unsupported boundary; do not imply broad legacy migration support.
- [x] Source: bundle uses `ResolvedContext::load`, exact source candidates, canonical relocation policy, selected variant copy, actual publisher destination handles, canonical save, private staging, and no-clobber final rename. Intentionally skipped packages retain their original handles. The old raw flat-JSON implementation is historical.
- [x] Source: copy validates selected indices before writes, stages nested payloads/common `.rez/include`, and returns actual destination variant indices/handles from locked publication. CLI copy/move forwards typed force/policy/rename/reversion options. Move publishes a complete copy before source-ignore, preserving the source payload.
- [x] Source: payload-cache execution integration exists. Resolve and context load call `update_package_cache`; Rex execution calls `execution_packages`, which uses exact variant handles and `touch_cached` to substitute cached roots transiently. Serialized handles/source bases retain original provenance.
- [x] Source: cache add uses canonical cachable policy, force, local/same-device/temp guards, payload copying, worker requests, and cleanup. Nullable config policy is retained; false is terminal and only null falls through.
- [ ] Reverify actual installed-CLI bundle/cache/copy/move consumers after P0, including custom definition JSON/YAML relocation, nested variants/includes, original skipped handles, non-relocatable rejection/force, repeated publication, failed move/source preservation, and frozen worker configuration.
- [ ] Extend cache acceptance for asynchronous lifecycle, concurrency, local/device/temp policy, identity/invalidation, corruption/error classification, and real cached payload execution. Preserve the distinction between payload caching and resolver-result caching.
- [ ] Complete bidirectional reference/native context fixtures for defaults/nulls/unknowns/errors, plugin handles, graph serialization, loaded-package metrics, public variant/failure/time-limit behavior, and the explicit legacy migration decision.
- [ ] Accept live Memcached multiple-server, restart, timeout, and malformed-protocol behavior.

Existing regression anchors include `cached_roots_are_transient_and_input_policy_precedes_cache_update` and `exact_combined_handles_preserve_source_identity_through_cache_collisions` in context.rs, plus the five bundle/cache/copy/worker/root-flag consumers in [CLI integration tests](../../tests/cli_integration.rs). The saved workspace campaign covers source test scopes; broader installed/platform acceptance remains separate.

## A3 — Native builds, ecosystem installers, and Python recipe APIs

Current owner: [build-system adapters and orchestration](../../crates/rez/build-system/src/builders/mod.rs), with [native artifact preparation](../../crates/rez/build-system/src/builders/native.rs), [Pip SourceMap](../../crates/rez/build-system/src/builders/pip_utils.rs), and [distribution finalization](../../crates/rez/build-system/src/pip/metadata.rs).

- [x] Source: 15 local adapters exist: CMake, Make, Python, Pip, Cargo, Go, Zig, NodeJs, Bun, SCons, Vcpkg, Conan, Extraction, Custom, and NoOp. Existing BuildContext/BuildResult and owned prepared payloads feed the shared publisher after selected variants succeed.
- [x] Source and scoped consumers: local Cargo/Go/Pip/CMake fixtures exist in [native_builders](../../tests/native_builders.rs); the saved workspace composite includes the fresh four-test native scope and two separately executed builder tests. CMake production integration is present; its local fixture is no longer an unexecuted source-only item.
- [x] Source: Pip sparse SourceMap preserves declared sibling coordinates, uses the selected parser/interpreter/runner through discovery/execution, validates known launchers, and restores generated editable metadata. `Distribution::finalize` regenerates installed RECORD paths, hashes, and sizes after transformations. Stale RECORD and wholly absent sibling mapping describe earlier snapshots.
- [x] Source: vcpkg uses effective ROOT/PATH, owned install/buildtrees/packages directories, response-file/passthrough guards including arguments after `--`, and prepared publication. Planning/download-only modes do not imply installation.
- [ ] Run actual vcpkg acceptance, covering owned roots, response files, cache-only/no-install modes, failure preservation, and publication. Source guard tests do not substitute for the tool.
- [ ] Extend Pip external relative-input/source-graph acceptance through real requirements, relative find-links, sibling wheel/source projects, legacy setup inputs, selected wrappers, editable import after work drop, and installed RECORD validation. Local regular/editable acceptance does not exhaust these cases.
- [ ] Accept actual Cargo/Go/Zig local/git/archive recipes and exact command/environment/artifact contracts: Rust `idle_rs` binary/cdylib/site-packages/resources; Go launch_handler/AMI/resources/cgo; Zig recipe versus ecosystem optimization policy.
- [ ] Complete common typed options/artifact/acquisition/offline/checksum contracts across adapters; specify Node/Bun missing/empty output behavior and public passthrough boundaries.
- [ ] Derive Nim application, .NET, Xcode, and uv packaging requirements from actual consumers. Nim/.NET/Xcode are not members of the current BuildSystemType enum; uv sync/run alone does not establish packaging.
- [ ] Implement/accept standalone ecosystem dispatch separately from local `rez build`: shared identity/version precedence/provenance/destination/list/remove/publication and SemVer/range conversion, including caret 0.x, prerelease/build metadata, and observed versus pinned versions.
- [x] Add an initial CPython compatibility facade under `rez.version`, `rez.packages`, and `rez.resolved_context`, with native additions exclusively in `rez.rs`. Ownership and explicit unsupported surfaces are documented in [Python API](../mdbook/src/python-api.md). This does not supply the complete upstream Python API.
- [ ] Resolve the observed external-CPython `rez.cli._main` import boundary for concrete recipe/toolkit consumers. Frozen embedded modules and native alias visibility do not install the Rez Python package into external CPython.

The 2026-10-03 user decision remains: private `rez.bld` namespace/signature parity is optional. Preserve required behavior through existing Rust adapters; choose Python compatibility only for an identified consumer. Original Rez core Python API and standalone ecosystem obligations remain.

## A4 — Customized extension policies

The following 22 extension groups remain in scope. Current [config](../../crates/rez/model/src/config.rs), Package policy, context suite/implicit selection, and typed CLI consumers are shared implementations; historical flags/default/source-test findings must be checked against those owners.

| Original groups | Remaining boundary |
|---|---|
| 1, 2, 21: categories, scratch path, location | Actual builder/deployment consumers in A3/A6 |
| 5, 7: platform baseline, clean children | Full provenance/native shell behavior in A7 |
| 9, 17: Rez deployment/rebinding | Current activation and 1055 replay in A6 |
| 10, 18, 19: PowerShell, Cmd, aliases | Full renderer/transport matrix in A7 |
| 15, 16: Python helpers/ecosystems | Concrete API/dispatch contracts in A3 |
| 3: post-success scratch cleanup | Containment and failure semantics for optional policy |
| 4: cache-copy backend | Measure modes while preserving shared transaction invariants |
| 6: argument groups | Every public passthrough boundary, preserving groups |
| 8: junction identity | User-visible paths versus canonical package identity |
| 11: Pip defaults/prefix/markers/locking | Remaps/Git/upload/ACL/metadata-skip consumers |
| 12: parent VCS lookup | Discovery and consuming release commands |
| 13: hashed variant shortlinks | Configured creation and resolution; named variants default false |
| 14: optional release ordering | Reference default order and opt-in shared orderers |
| 20: cache queue/logger | Local-variant skip and handler lifecycle |
| 22: Windows Python launchers | Native packaging and installed consumers |

- [x] Source: strict public config types/default writers, nullable policy maps, canonical `REZ_REZ_TOOLS_VISIBILITY`, selected implicit requirements, suite visibility, and false default hashed variants are retained. Existing config and behavioral shell tests accompany these boundaries.
- [ ] Close remaining rows through current source/call-site evidence and actual consumers; preserve public config defaults/precedence/errors/collision checks.
- [ ] Retain all 22 dispositions; avoid private customized aliases and package-specific substitutes for shared behavior.

## A5 — Builders, acquisition, publication, binds, and release

Current owners: [foundation filesystem copy](../../crates/rez/foundation/src/filesystem.rs), [generated path/file checks](../../crates/rez/foundation/src/util.rs), [download](../../crates/rez/build-system/src/builders/download.rs), [extraction](../../crates/rez/build-system/src/builders/extraction.rs), [repository publisher](../../crates/rez/repository/src/repository.rs), and [native binds](../../crates/rez/repository/src/package/bind/mod.rs).

- [x] Source: the shared copier has declared destination authority, generated-ancestor checks, atomic file-leaf replacement, opaque symlink handling, and postorder file/directory stat preservation. Link-stat parity and descriptor-relative hostile ancestor-swap protection are not established by those checks.
- [x] Source: extraction stages archive inputs before shared merge/publication; HTTP acquisition has URL-scoped locks, validated resume/checksum state, and bounded hash-derived control filenames. This code is no longer an uncompiled source-only tranche.
- [x] Source: canonical publication returns actual variant mappings under the persistent version lock, journals owned payload changes, and writes metadata after preparation. Native binder routes through that publisher.
- [ ] Complete HTTP status/Range/416/redirect/body/checksum/concurrency acceptance and archive tar/MSI/symlink/permissions/merge boundaries, including generated redirects and interrupted downloads.
- [ ] Accept custom Python bind options/ranges/recursive dependencies, configured stems/format/all variants, hooks, DCC, AMQP, and release consumers.
- [ ] Finish writer/publisher audit and native non-UTF8/cross-volume/filesystem-stat/reparse/rollback acceptance. Existing-payload callbacks do not promise rollback of arbitrary writes, and a locked writer does not establish an atomic snapshot for unlocked readers.
- [ ] Reverify copy/move force, policy, selected rename/reversion, timestamp distinction, and failure preservation at shared and installed CLI boundaries. Preserve metadata-last/journal guarantees across adapters.

## A6 — Complete Bootstrap117 deployment

**Execution owner:** the user launches external Bootstrap package recipes. Agent scope remains source repairs, isolated tests, and verified fresh CLI deployment; do not launch the external installation queue as part of this documentation task.

- [ ] Recheck seed/cache/payload availability after cleanup.
- [ ] Accept standalone/C:/rez3/repository destinations, full 1055 replacement/replay, legacy ownership/PATH, and journaled activation with a current accepted package.
- [ ] Verify scheduler AND/self/dependency readiness and every nested/version leaf across all 20 groups.
- [ ] Invoke actual builder launchers and validate PySide6/PyQt6, xmem2/OIIO, Blender, Houdini MOPS, and later application consumers.
- [ ] Resolve exact NumPy 2.5.2 and Python 3.14 coverage and the recorded zero-byte studio CA payload through real sources. Recheck historical missing MOPS cache before treating it as a current filesystem fact.
- [ ] Record whole-pipeline installed consumers and per-entry failures/results.

Complete groups retained: 0010.prep, 0030.python, 0040.tools, 0045.tools_extra, 0050.pipeline_tools, 0055.custom_apps, 0060.nuke, 0070.blender, 0080.houdini, 0090.c4d, 0100.adobe, 0110.maya, 0120.sg, 0125.rv, 0130.ue, 0140.redshift, 0150.keentools, 0160.deadline, 0170.custom_tools, and 0180.borisfx. Record the detailed leaf ledger with future acceptance here. External bootstrap edits require that tree's backup/CHANGELOG conventions.

## A7 — Shells, GUI, and native platforms

### Active Rex contract workstream

Current owners: [context Python preamble](../../crates/rez/resolve/src/context.rs), [shared native action wire](../../crates/rez/rex/src/wire.rs), [Rex manager/interpreters](../../crates/rez/rex/src/rex.rs), and [embedded runtime/exception bridge](../../crates/rez/python-runtime/src/lib.rs).

- [x] Source: `setenv`, `unsetenv`, `resetenv`, `prependenv`, `appendenv`, `format`, `expandvars`, `optionvars`, and `shebang` are present. Global mutations and variable proxy methods use `_env_action`; the mapping protocol remains implemented.
- [x] Source: namespace formatting is in the shared Python preamble; environment queries/expansion use the native manager through `rex_environ`. Shared `apply_rex_actions` validates fields/segments and rejects unknown actions. It is inaccurate to call these helpers or decoding wholly absent.
- [x] Source and scoped reference vectors: literal/expandable segments, reference error identity, System/Requirement/Version bindings, interpreter-specific package root/base normalization, environment/error corpora, and public binding isolation have regression coverage.
- [ ] Finish signatures/defaults/error/falsey/missing-value semantics and importable public Python RexExecutor/ActionManager/ActionInterpreter/Python/NamespaceFormatter API contracts. Package preamble helpers do not establish these modules.
- [ ] Complete literal/expandable/mixed provenance through File/Eval/direct/batch/CALL/suite/test/export consumers, actual Cmd/PowerShell, and native Bash/Zsh/Csh/Tcsh/detection behavior.
- [ ] Accept GUI interaction, persistence, errors, graph behavior, and export against the updated nodes-rs dependencies.
- [ ] Accept Linux/macOS filesystem, ELF/Mach-O rewrite/load/safety, and native binary-loader consumers. Retain the four ELF TODOs for segment resizing/reuse, contiguous-section verification, and dynamic-entry references until behavioral evidence closes them.

Extend the shell, GUI, and binary-patch matrices through actual consumers. Complete recipe installation remains A6.

## A8 — Repository inventory and artifact closure

- [x] Source: functional crate ownership and dependencies are documented; CLI imports owning crates directly. The saved source inventory retained all 1,511 original test functions and added three callback tests; source counts are not executed-test counts.
- [ ] Finish module/feature/API/plugin/ignored-test/TODO inventory and ownership/deduplication crosswalk. Inspect callers and unfinished intent before deleting code.
- [x] Re-run current-source workspace compiler/runtime/strict Clippy/doc-tests and the six native fixtures after the migration, preserving prior failures and their historical composite boundaries. Wider native/platform consumers remain in A1–A7.
- [x] Regenerate canonical release/source packaging after document freeze; verify metadata, all member bytes/membership/CRC, installer helper equality, and exclusion of VCS/generated content.
- [x] Record exact source/dependency/artifact hashes and commands in `dist/verification.json`. These establish the packaged Windows CLI scope, not current host installation or complete compatibility.

## Recording future work

For each remaining task, inspect current source and the concrete reference/consumer, check graph freshness and impact before symbol edits, use its owning model/config/build/publication boundary, run the necessary authorized scoped gate, and record the source snapshot/result/consumer/platform limits here. Historical agent aliases and source freezes do not assign new work.

Documentation validation checks current local links and anchors. Compiler, runtime, packaging, installation, and platform results retain their separate acceptance boundaries.
