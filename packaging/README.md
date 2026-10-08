# Package installer source

`install.py` is the canonical installer shipped with a staged rez-rs release. `python bootstrap.py p --force` copies it to `dist/install.py` beside `cli_install.py`, `system_bind.py`, and the `rez_rs` payload.

Edit this source rather than the generated dist copy. The installer requires an explicit `--repository-root`; `--rez-root` additionally activates the host CLI. Repository-only installation leaves host activation untouched.

The root `bootstrap.py` builds and stages releases. This directory owns the installer that consumes that staged release; it is part of the current packaging flow.
