# Contributing

Thank you for considering a contribution. Vibe Factory is meant to be extended, and the
best contributions are often plugins and agents rather than changes to the core.

## Ways to contribute

- **Report bugs and propose features** in GitHub issues. Include the `vibe doctor` output
  and, for pipeline problems, the `events.jsonl` of the task.
- **Write a plugin.** See [Plugins](https://vincentlauriat.github.io/vibe-factory/user/plugins.html). Plugins live in their own
  repository; open an issue to have yours listed.
- **Improve an agent prompt.** Prompts live in `crates/vibe-agents/src/prompts/`. Explain
  the failure mode you observed and how the change fixes it.
- **Improve the documentation.** Every page of this book has an *edit* link.
- **Change the framework.** Open an issue first for anything that touches a `vibe-core`
  trait; trait changes must stay backwards compatible (new methods get default bodies).

## Development setup

```sh
git clone https://github.com/vincentlauriat/vibe-factory
cd vibe-factory
cargo build --workspace
cargo test --workspace
cargo install mdbook && mdbook serve docs
```

## Quality gates

CI runs on Linux, macOS and Windows:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets   # with RUSTFLAGS="-D warnings"
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
mdbook build docs
```

Every public item needs a doc comment. `unsafe` is forbidden. Tests must not depend on
network access or on the developer's global git configuration.

## Pull requests

- Target `develop`; `main` is for releases.
- One logical change per PR, with tests and documentation updates.
- Use conventional commit messages (`feat(tools): …`, `fix(pipeline): …`, `docs: …`).
- Significant design changes get an ADR in `docs/src/design/adr/`.

## Code of conduct

This project follows the [Contributor Covenant](https://www.contributor-covenant.org);
see `CODE_OF_CONDUCT.md`.
