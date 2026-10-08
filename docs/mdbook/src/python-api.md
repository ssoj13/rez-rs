# Python API

The separate CPython distribution provides an experimental compatibility facade in the usual Rez namespaces. Rust-specific functions live in `rez.rs`. The facade delegates version ordering, package loading, repository provenance, solving, Rex environments, and context serialization to the existing Rust owners.

This is an initial supported subset, not full upstream API parity. Use a separate Python environment from an existing upstream Rez installation: both distributions provide the `rez` package. No upstream Python checkout is bundled.

## Build and import

Run packaging with CPython 3.10 or newer with the GIL enabled:

```console
python bootstrap.py p --force
```

The CLI/source package stays in `dist/rez_rs/<version>`. The Python distribution is separate:

```text
dist/python/<version>/
  rez/
    __init__.py
    rs.pyd
    version.py
    packages.py
    resolved_context.py
    config.py
    exceptions.py
    status.py
  LICENSE
  NOTICE
  third-party-licenses/
  python-api.json
  rez-rs-python.zip
```

On Unix the extension filename is `rs.abi3.so`. Windows x86_64 is the initial acceptance platform; native Unix runtime acceptance remains open. The extension uses the CPython stable ABI with a 3.10 minimum. Free-threaded Python builds are outside this distribution's supported scope.

Add the directory containing `rez/` to Python's module search path. On Windows:

```powershell
$env:PYTHONPATH = (Resolve-Path dist/python/0.1.0).Path
python -c "from rez.version import Version; from rez import rs; print(Version('1.2'), rs.__version__)"
```

Alternatively extract `rez-rs-python.zip` and add its extraction directory to the search path. Python cannot load a `.pyd` directly from a ZIP file. Keep `rs.pyd` inside `rez/` alongside the wrappers. The CLI staged-release installer installs the CLI/source package; it does not alter an existing Python environment.

A CPython installation is required to import the extension. The CLI remains self-contained and does not require this Python distribution for configuration, binding, or its embedded interpreter.

## Supported compatibility surface

| Namespace | Initial surface |
|---|---|
| `rez.version` | Alphanumeric tokens, Version comparison/hash/components/copy/trim/next, VersionRange membership and set operations, Requirement parsing/conflicts, VersionedObject |
| `rez.packages` | Family/package iteration, exact/latest lookup, request-string lookup, developer metadata, Package/Variant metadata, requirements, selected roots and variant handles |
| `rez.resolved_context` | Resolving requests, status/failure inspection, requested/resolved packages, environment reconstruction, canonical .rxt save/load, current-context loading |
| `rez.config` | Read-only canonical configuration snapshot |
| `rez.exceptions` / `rez.status` | Backend exception classes and resolver statuses |

Example:

```python
from rez.packages import get_latest_package
from rez.resolved_context import ResolvedContext
from rez.version import VersionRange
from rez import rs

paths = ["C:/packages"]
package = get_latest_package("example", VersionRange("1+<2"), paths=paths)
context = ResolvedContext(
    ["example-1"], package_paths=paths,
    add_implicit_packages=False, caching=False,
)
if context.success:
    environment = context.get_environ()
    context.save("example.rxt")
else:
    print(context.failure_description)

# Direct native access returns plain Python metadata mappings.
print(rs.version_info("1.2"))
```

`get_environ(parent_environ)` returns variables produced by Rex; the parent supplies inherited values for lookup and expansion. It does not copy every unchanged parent variable into its result.

Configure `REZ_CONFIG_FILE` and other Rez environment settings before the first backend configuration access. The configuration owner captures process settings once; `config.override` is explicitly unsupported.

Custom token factories, permissive bound parsing, repository/plugin wrapper objects, context-bound deferred Python metadata, custom solver callbacks/filters/orderers, subprocess/shell execution, testing and package-cache options, and the complete upstream `rez.cli` / build / shell / utility API remain open work. Unsupported exposed options raise `NotImplementedError`; absent APIs are not silently emulated. The private `rez.bld` API is still outside the accepted scope.

## Native namespace

`rez.rs` is the compiled extension. It exposes version/range/requirement primitives, `packages`, `package_families`, `developer_package`, `package_from_handle`, `resolve_context`, `context_environ`, `context_save`, `context_load`, and `config_snapshot`. Metadata results are ordinary dictionaries and lists. Serialized context inputs use the canonical context JSON document, not a second context schema.

`crates/rez/python-api` owns the extension and facade. It uses PyO3 0.29.3; no version/solver algorithm is duplicated in the wrappers. Packaging sets `PYO3_BUILD_EXTENSION_MODULE=1` only for the extension build, allowing ordinary Rust tests and Clippy to retain their normal linking behavior. Cargo tests can rebuild the shared DLL with their linking settings; run packaging after tests and Clippy so the staged extension matches the final Cargo output. CI follows this order.

## Distribution verification

```console
python ci/verify_python_api.py
```

Verification compares extension bytes with Cargo output, checks archive membership/CRC and every wrapper's canonical bytes, then extracts the archive into a temporary directory. An isolated CPython process runs the acceptance suite against that extracted package. Tests cover versions, package definitions, variants, exact repository precedence, solving/failures, Rex environment commands, and saved context round trips.

Local Windows acceptance passed 14 tests on each of CPython 3.13.11 and 3.10.18 against the same abi3 archive; all 14 archive members matched canonical bytes and CRCs. CI is configured to run the same Windows archive on CPython 3.13 and 3.10 before upload. Read-only deploy keys are configured for the private GUI dependencies; hosted Windows acceptance is still a separate pending gate. The `windows-python` artifact and tagged releases contain `rez-rs-python.zip`, its SHA-256 checksum, and a verification receipt. These checks establish the listed consumers, not complete Rez compatibility or Unix acceptance.
