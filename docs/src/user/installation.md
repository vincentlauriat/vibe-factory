# Installation

Vibe Factory is a single binary, `vibe`, built from source with cargo. It runs on macOS,
Linux and Windows.

## Requirements

| Requirement | Version | Why |
|-------------|---------|-----|
| Rust toolchain | 1.88 or newer (edition 2024) | to build `vibe`; install with [rustup](https://rustup.rs) |
| git | any recent version | task workspaces are git worktrees; also needed by `cargo install --git` |
| a model provider | | an Anthropic or OpenAI key, any OpenAI-compatible endpoint, or a local Ollama |
| Ollama (optional) | | to run models locally without a key |

Check what you have:

```sh
rustc --version     # rustc 1.88.0 or newer
git --version
```

If `rustc` is older, run `rustup update stable`.

The project you point `vibe` at should be a git repository: the default workspace creates a
worktree per task. For a directory without git, use the `in_place` workspace (see
[Workspaces and merging](workspaces.md#in-place-mode)).

## Installing from git

```sh
cargo install --git https://github.com/vincentlauriat/vibe-factory vibe-cli
```

This builds the `vibe-cli` package in release mode and places the `vibe` binary in
`~/.cargo/bin` (`%USERPROFILE%\.cargo\bin` on Windows), which rustup adds to your `PATH`.
The first build downloads and compiles the dependencies and takes a few minutes.

To install a tagged release or a branch (replace the tag with one that exists):

```sh
cargo install --git https://github.com/vincentlauriat/vibe-factory --tag v0.1.0 vibe-cli
cargo install --git https://github.com/vincentlauriat/vibe-factory --branch main vibe-cli
```

## Building from source

```sh
git clone https://github.com/vincentlauriat/vibe-factory
cd vibe-factory
cargo build --release -p vibe-cli
./target/release/vibe --version
```

To install your local checkout as `vibe`:

```sh
cargo install --path crates/vibe-cli
```

The rest of the workspace builds and tests the same way:

```sh
cargo build --workspace
cargo test --workspace
mdbook serve docs          # this book, on http://localhost:3000
```

The repository also contains an example plugin, built separately:

```sh
cargo build --release -p vibe-plugins --bin vibe-echo-plugin
```

## Verifying the installation

```sh
vibe --version
```

```text
vibe 0.1.0
```

Then, inside a project:

```sh
cd your-project
vibe init
vibe doctor
```

`vibe doctor` checks git, the project repository, the configuration, provider keys,
plugins, the workspace setting and that `.vibe/` is writable, one line per check. A missing key is expected at
this point if you have not set one yet; the [Quick start](quickstart.md) continues from here.

## Setting up a model provider

With the default configuration, `vibe` uses `anthropic/claude-sonnet-5` and reads the key
from `ANTHROPIC_API_KEY`:

```sh
export ANTHROPIC_API_KEY=sk-ant-…          # bash, zsh
set -gx ANTHROPIC_API_KEY sk-ant-…         # fish
$env:ANTHROPIC_API_KEY = "sk-ant-…"        # PowerShell
```

For OpenAI, set `OPENAI_API_KEY` and a default model:

```sh
export OPENAI_API_KEY=sk-…
vibe config set default_model openai/gpt-5
```

For a local model with Ollama, no key is needed:

```sh
ollama pull qwen2.5-coder
ollama serve                                # if not already running as a service
vibe config set default_model ollama/qwen2.5-coder
```

Other vendors and gateways are configured in [Providers and models](providers.md).

## Shell completions

`vibe completions <shell>` prints a completion script for `bash`, `zsh`, `fish`, `elvish`
or `powershell`.

| Shell | Install |
|-------|---------|
| bash | `vibe completions bash > ~/.local/share/bash-completion/completions/vibe` |
| zsh | `vibe completions zsh > ~/.zfunc/_vibe`, with `fpath+=~/.zfunc` before `compinit` in `~/.zshrc` |
| fish | `vibe completions fish > ~/.config/fish/completions/vibe.fish` |
| PowerShell | `vibe completions powershell >> $PROFILE` |

Open a new shell afterwards. Regenerate the script after upgrading, since new options only
appear in a fresh script.

## Upgrading

Run the install command again with `--force`:

```sh
cargo install --force --git https://github.com/vincentlauriat/vibe-factory vibe-cli
```

From a checkout: `git pull` then `cargo install --path crates/vibe-cli --force`.

Project data is forward-compatible: fields added to persisted files have defaults, so tasks
created by an older version still load. Read the changelog before upgrading across a minor
version, and finish or pause running tasks first. Your `.vibe/config.toml` is never rewritten
by an upgrade; `vibe config show --default` shows the new defaults so you can compare.

## Uninstalling

```sh
cargo uninstall vibe-cli
```

This removes the binary only. Per-project data lives in each project's `.vibe/` directory;
discard task worktrees first so that their branches are removed too, then delete the
directory:

```sh
vibe task list --all              # tasks whose worktrees you may want to discard
vibe task discard <ref> --yes     # for each task
rm -rf .vibe
git worktree prune
```

User-level plugins live in your configuration directory (`~/.config/vibe/` on Linux,
`~/Library/Application Support/vibe/` on macOS, `%APPDATA%\vibe\` on Windows); remove it if
you installed any.

## Platform notes

**macOS and Linux.** The `bash` tool runs commands with `sh -c` in the workspace, in their
own process group, so a timeout kills the whole process tree. Any POSIX `sh` works; the
commands agents write (`cargo test`, `npm test`, `grep`, …) must be installed on your
machine, as for a human developer.

**Windows.** The `bash` tool runs commands with `cmd /C`, and a timeout kills the process
tree with `taskkill /T /F`. The security policy still parses commands with POSIX shell
rules, so constructs specific to `cmd` (for example `%VAR%` expansion or `^` escapes) may be
rejected as unparseable; commands such as `cargo test` or `git status` work unchanged.
Programs must be on `PATH`. Paths given to tools are resolved against the workspace and must
stay inside it, as on other platforms, and a bare drive root (`C:\`) is never a valid target
for `rm`. Long paths inside deep worktrees
can hit the 260-character limit: enable long paths in git
(`git config --global core.longpaths true`) and in Windows, or keep the project close to the
drive root.

**All platforms.** Worktrees live inside the project, under `.vibe/worktrees/`, so they are
on the same file system as your checkout and need roughly one extra working copy of disk
space per open task (see [Workspaces and merging](workspaces.md#disk-usage)).
