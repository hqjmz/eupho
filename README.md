# Eupho

Eupho is a GitHub-native control plane for coding agents. GitHub owns durable workflow state and merge policy; Claude Code, Codex, OpenCode, and future runners are replaceable workers; Otty or another terminal is an optional viewport.

This repository currently implements the Phase 1 observe-only spine:

- validated repository and administrator configuration;
- a pure, tested run state machine;
- signed run metadata primitives and rollback guards;
- durable local run storage and repository locking;
- execution-mode routing for attended worktrees and unattended clones;
- runner and workspace ports for later adapters;
- a `doctor` command for local and optional GitHub policy checks;
- a read-only `once` command that discovers ready issues and prints the actions Eupho would take;
- a `status` command for durable local runs;
- an `instructions link` command for safely sharing one instruction file between Claude Code and Codex.

Phase 1 never claims issues, changes labels, creates branches, or writes to GitHub.

## Requirements

- Rust 1.85 or newer
- Git
- GitHub CLI (`gh`) for GitHub-backed commands
- A GitHub operator token in `EUPHO_DOCTOR_TOKEN` only when running strict branch-protection diagnostics

The diagnostic token is separate from the future runtime GitHub App credential. It may have read-only Administration access; runner processes never receive either credential.

Without a host configuration, observe-only snapshots use the OS user-state directory (`$XDG_STATE_HOME/eupho` when set, otherwise the platform home state directory), never the repository working tree. Production use should supply an administrator-owned host configuration.

## Quick start

```bash
cargo install --path . --locked
eupho doctor
```

Inspect a repository without mutating it:

```bash
eupho once --repo OWNER/REPOSITORY
```

Run strict GitHub policy diagnostics:

```bash
EUPHO_DOCTOR_TOKEN=... eupho doctor \
  --repo OWNER/REPOSITORY \
  --host-config config/examples/host.yml
```

Use `--json` with `doctor`, `once`, or `status` for machine-readable output.

Keep Claude Code and Codex project instructions in sync with one source file:

```bash
# Default: CLAUDE.md -> AGENTS.md
eupho instructions link

# Reverse direction: AGENTS.md -> CLAUDE.md
eupho instructions link --source claude
```

The source file must already exist. Eupho creates a relative symbolic link, is
idempotent when the correct link already exists, and never overwrites an
existing file, directory, or unexpected link.

## Configuration

Repository policy is loaded from the first existing path below unless `--config` is supplied:

1. `.github/eupho.yml`
2. `.github/agent-orchestrator.yml`

The checked-in [.github/eupho.yml](.github/eupho.yml) is a safe starter. Administrator-owned host settings are separate; see [config/examples/host.yml](config/examples/host.yml). Repository policy may name predeclared host profiles but cannot supply credential paths, signing keys, executable hooks, or arbitrary host paths.

The sample policy intentionally includes one narrow autonomous class (`agent:risk:docs-only`). Phase 1 only reports that route. Later phases must independently enforce its changed-path allowlist before publishing a successful review or enabling merge.

## Commands

```text
eupho doctor [--repo OWNER/REPO] [--config PATH] [--host-config PATH] [--json]
eupho once --repo OWNER/REPO [--config PATH] [--host-config PATH] [--json]
eupho status [--state-root PATH] [--host-config PATH] [--json]
eupho instructions link [--source agents|claude] [--path PATH] [--json]
eupho help
```

## Architecture

See [docs/architecture.md](docs/architecture.md) for the initial package map and implementation boundary. [SPEC.md](SPEC.md) is the reviewed source of truth for later phases.

## Development

```bash
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
```

Rust is the sole implementation language and runtime for the Eupho backend.
