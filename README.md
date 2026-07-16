<div align="center">

<h1>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://media.x.ai/v1/website/spacexai-symbol-white-transparent-0c31957f.png">
    <source media="(prefers-color-scheme: light)" srcset="https://media.x.ai/v1/website/spacexai-symbol-black-transparent-6435cf42.png">
    <img alt="SpaceXAI logo" src="https://media.x.ai/v1/website/spacexai-symbol-black-transparent-6435cf42.png" width="96">
  </picture>
  <br>
  Grok Build (<code>grok</code>)
</h1>

**Grok Build** is SpaceXAI's terminal-based AI coding agent. It runs as a
full-screen TUI that understands your codebase, edits files, executes shell
commands, searches the web, and manages long-running tasks — interactively,
headlessly for scripting/CI, or embedded in editors via the Agent Client
Protocol (ACP).

[Remote control (Tailscale)](#remote-control-via-tailscale) ·
[Installing the released binary](#installing-the-released-binary) ·
[Building from source](#building-from-source) ·
[Documentation](#documentation) ·
[Repository layout](#repository-layout) ·
[Development](#development) ·
[Contributing](#contributing) ·
[License](#license)

![Grok Build TUI](https://media.x.ai/v1/website/universe-tui-screenshot-6f7a0837.png)

**Learn more about Grok Build at [x.ai/cli](https://x.ai/cli)**

This repository contains the Rust source for the `grok` CLI/TUI and its agent
runtime. It is synced periodically from the SpaceXAI monorepo.

</div>

---

## Remote control via Tailscale

Drive a live Grok session from your phone or another computer on your private
[Tailscale](https://tailscale.com) network. Execution stays on the host machine
(files, tools, MCP); the phone is a stream-and-steer web UI.

### What you need

| Where | Requirement |
|-------|-------------|
| **Host** (runs Grok) | Tailscale installed, logged in, connected |
| **Phone / other device** | Tailscale app, **same Tailscale account** as the host |
| **Grok** | Built from this tree (or a build that includes `/remote`) |

### 1. Install and start Tailscale

**Host (macOS example):**

```sh
brew install --cask tailscale
# Open the Tailscale app → Log in
tailscale status    # should show Running and a 100.x IP
# or: tailscale up
```

**Linux:**

```sh
curl -fsSL https://tailscale.com/install.sh | sh
sudo tailscale up
tailscale status
```

**Phone:** install Tailscale from the App Store / Play Store and log into the
**same account** as the host.

### 2. Build and run Grok from this repo

Requirements: **Rust** (`rustup`, pinned by `rust-toolchain.toml`) and
**protoc** (or [dotslash](https://dotslash-cli.com) so `bin/protoc` works).

```sh
git clone https://github.com/daniel-farina/grok-build.git
cd grok-build

# if protoc is missing:
#   brew install protobuf          # macOS
#   or: cargo install dotslash

cargo run -p xai-grok-pager-bin
# release binary:
# cargo build -p xai-grok-pager-bin --release
# ./target/release/xai-grok-pager
```

On first launch, authenticate in the browser as usual.

### 3. Enable remote control

In an **active session** (start chatting first):

```text
/remote
```

Aliases: `/rc`, `/remote-control`.

Grok will:

1. Check that Tailscale is installed and connected (if not, print install hints)
2. Start a small web UI on this machine (default port `7788`)
3. Print a **URL** and **QR code**

Example URL shape:

```text
http://100.x.x.x:7788/s/<secret-token>/
```

### 4. Connect from your phone

1. Confirm Tailscale is on and using the **same account** as the host  
2. Open the printed URL (or scan the QR code) in a mobile browser  
3. Stream the session and type messages to steer the agent  

Local TUI and phone share **one session** (dual input). Desktop messages show
as “You (desktop)” on the phone; phone messages appear in the TUI as
`← remote (Tailscale)`.

### Notes

- **Not cloud execution** — tools and files stay on the host; the host process
  must keep running.
- **Security** — reachable on your tailnet; URL includes a secret path token.
  Prefer keeping Tailscale ACLs tight.
- Re-run `/remote` anytime to re-show the URL/QR. Remote stops when Grok exits.
- More detail: [slash commands — `/remote`](crates/codegen/xai-grok-pager/docs/user-guide/04-slash-commands.md)

---

## Installing the released binary

Prebuilt binaries are published for macOS, Linux, and Windows:

```sh
curl -fsSL https://x.ai/cli/install.sh | bash   # macOS / Linux / Git Bash
irm https://x.ai/cli/install.ps1 | iex          # Windows PowerShell
grok --version
```

See the [changelog](https://x.ai/build/changelog) for the latest fixes,
features, and improvements in each release.

## Building from source

Requirements:

- **Rust** — the toolchain is pinned by [`rust-toolchain.toml`](rust-toolchain.toml);
  `rustup` installs it automatically on first build.
- **protoc** — proto codegen resolves [`bin/protoc`](bin/protoc) (a
  [dotslash](https://dotslash-cli.com) launcher) or falls back to a `protoc` on
  `PATH` / `$PROTOC`.
- macOS and Linux are supported build hosts; Windows builds are best-effort
  and not currently tested from this tree.

```sh
cargo run -p xai-grok-pager-bin              # build + launch the TUI
cargo build -p xai-grok-pager-bin --release  # release binary: target/release/xai-grok-pager
cargo check -p xai-grok-pager-bin            # fast validation
```

The binary artifact is named `xai-grok-pager`; official installs ship it as
`grok`. On first launch it opens your browser to authenticate — see the
[authentication guide](crates/codegen/xai-grok-pager/docs/user-guide/02-authentication.md).

## Documentation

Full online documentation is available at
[docs.x.ai/build/overview](https://docs.x.ai/build/overview).

The user guide ships with the pager crate:
[`crates/codegen/xai-grok-pager/docs/user-guide/`](crates/codegen/xai-grok-pager/docs/user-guide/)
— getting started, keyboard shortcuts, slash commands, configuration, theming,
MCP servers, skills, plugins, hooks, headless mode, sandboxing, and more.

## Repository layout

| Path | Contents |
|------|----------|
| `crates/codegen/xai-grok-pager-bin` | Composition-root package; builds the `xai-grok-pager` binary |
| `crates/codegen/xai-grok-pager` | The TUI: scrollback, prompt, modals, rendering |
| `crates/codegen/xai-grok-shell` | Agent runtime + leader/stdio/headless entry points |
| `crates/codegen/xai-grok-tools` | Tool implementations (terminal, file edit, search, ...) |
| `crates/codegen/xai-grok-workspace` | Host filesystem, VCS, execution, checkpoints |
| `crates/codegen/...` | The rest of the CLI crate closure (config, MCP, markdown, sandbox, ...) |
| `crates/common/`, `crates/build/`, `prod/mc/` | Small shared leaf crates pulled in by the closure |
| `third_party/` | Vendored upstream source (Mermaid diagram stack) — see below |

> [!IMPORTANT]
> The root `Cargo.toml` (workspace members, dependency versions, lints,
> profiles) is **generated** — treat it as read-only. Prefer editing per-crate
> `Cargo.toml` files.

## Development

```sh
cargo check -p <crate>        # always target specific crates; full-workspace builds are slow
cargo test -p xai-grok-config # per-crate tests
cargo clippy -p <crate>       # lint config: clippy.toml at the repo root
cargo fmt --all               # rustfmt.toml at the repo root
```

## Contributing

> [!NOTE]
> External contributions are not accepted. See [`CONTRIBUTING.md`](CONTRIBUTING.md).

## License

First-party code in this repository is licensed under the **Apache License,
Version 2.0** — see [`LICENSE`](LICENSE).

Third-party and vendored code remains under its original licenses. See:

- [`THIRD-PARTY-NOTICES`](THIRD-PARTY-NOTICES) — crates.io / git dependencies,
  bundled UI themes, and **in-tree source ports** (including openai/codex and
  sst/opencode tool implementations)
- [`crates/codegen/xai-grok-tools/THIRD_PARTY_NOTICES.md`](crates/codegen/xai-grok-tools/THIRD_PARTY_NOTICES.md)
  — crate-local notice for the codex and opencode ports (license texts +
  Apache §4(b) change notice)
- [`third_party/NOTICE`](third_party/NOTICE) — vendored Mermaid-stack index
