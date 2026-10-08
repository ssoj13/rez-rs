# Working with Multiple Packages

**Full guide:** [docs/mdbook/src/suites.md](../docs/mdbook/src/suites.md) — Suites chapter in the rez-rs book.

## Summary

| Need | Use |
|------|-----|
| Maya + Redshift in one session | `rez env maya-2024 redshift` |
| Houdini + Redshift in one session | `rez env houdini-20 redshift` |
| Maya, Houdini, Nuke from one PATH | **Suite** — see docs |
| Offline / portable | `rez bundle ctx.rxt /path` |

### Suite quick start

```bash
rez suite ~/suites/studio --create
rez env maya-2024 redshift -o /tmp/maya.rxt
rez suite ~/suites/studio --add /tmp/maya.rxt --context maya
export PATH="$HOME/suites/studio/bin:$PATH"
maya   # launches Maya+Redshift
```
