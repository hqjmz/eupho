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
- a `status` command for durable local runs.

Phase 1 never claims issues, changes labels, creates branches, or writes to GitHub.

## Requirements

- Node.js 22 or newer
- Git
- GitHub CLI (`gh`) for GitHub-backed commands
- A GitHub operator token in `EUPHO_DOCTOR_TOKEN` only when running strict branch-protection diagnostics

The diagnostic token is separate from the future runtime GitHub App credential. It may have read-only Administration access; runner processes never receive either credential.

Without a host configuration, observe-only snapshots use the OS user-state directory (`$XDG_STATE_HOME/eupho` when set, otherwise the platform home state directory), never the repository working tree. Production use should supply an administrator-owned host configuration.

## Quick start

```bash
npm install
npm run check
npm run build
node dist/src/cli.js doctor
```

Inspect a repository without mutating it:

```bash
node dist/src/cli.js once --repo OWNER/REPOSITORY
```

Run strict GitHub policy diagnostics:

```bash
EUPHO_DOCTOR_TOKEN=... node dist/src/cli.js doctor \
  --repo OWNER/REPOSITORY \
  --host-config config/examples/host.yml
```

Use `--json` with `doctor`, `once`, or `status` for machine-readable output.

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
eupho help
```

## Architecture

See [docs/architecture.md](docs/architecture.md) for the initial package map and implementation boundary. [SPEC.md](SPEC.md) is the reviewed source of truth for later phases.

## Development

```bash
npm run typecheck
npm test
npm run check
```

The project intentionally starts with Node's built-in test runner and a small dependency surface. `yaml` is the only runtime package.
