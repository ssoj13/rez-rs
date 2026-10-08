# Documentation

The user and developer book lives in [mdbook](mdbook/src/SUMMARY.md). Its source chapters, project notes, and configuration are kept together.

```bash
mdbook build docs/mdbook
mdbook serve docs/mdbook
```

Generated HTML goes to `docs/build/`. It is ignored by Git and excluded from source packages.

[Plan30](plans/plan30.md) is the only retained compatibility work plan. Previous plans and audit reports were removed from this source snapshot. The plan supports development and is outside the book's navigation.

Source references and links to the work queue in the rendered book point to the repository. They use the repository URL declared in Cargo.toml. Agent instructions use `AGENTS.md`.
