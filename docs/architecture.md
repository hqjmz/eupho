# Eupho architecture

Eupho uses a ports-and-adapters structure so orchestration policy does not depend on an agent CLI, terminal product, or GitHub transport.

```text
src/
  main.rs          command parsing and rendering
  application.rs   doctor, policy resolution, observe-once, and status flows
  config.rs        repository policy and administrator configuration
  domain.rs        run record, state machine, and issue routing
  github.rs        read-only GitHub port and gh-backed adapter
  infra.rs         atomic storage, run store, state-root safety, and OS locks
  instructions.rs  safe AGENTS.md / CLAUDE.md link management
  runner.rs        product-neutral runner port
  security.rs      canonical JSON, HMAC envelopes, and revision guards
  workspace.rs     attended-worktree and unattended-clone port
```

The backend is a single Rust binary. External programs are always launched with
argument arrays; repository and GitHub text is treated as untrusted data.

## Phase 1 boundary

Phase 1 is deliberately read-only with respect to GitHub. `once` discovers issues carrying the configured ready label, selects a planned execution mode, and renders the plan. It does not claim, label, branch, comment, or launch an agent.

`doctor` has two levels:

- local checks validate the Eupho build, Git, GitHub CLI availability, repository policy, and optional host configuration;
- `doctor --repo` performs strict GitHub checks using `EUPHO_DOCTOR_TOKEN`, an operator credential separate from the runtime App. It checks the ready label, strict required checks, expected App source, and stale-approval dismissal where applicable.

## Next implementation slice

The next safe slice is attended authoring:

1. acquire the repository lock;
2. claim one ready issue through a GitHub write port;
3. create a marked linked worktree;
4. launch one runner adapter with native permission prompts;
5. freeze and independently validate the snapshot;
6. publish a draft pull request through dispatcher-only credentials;
7. preserve human final approval.

Unattended execution, App-owned Check Runs, cross-agent repairs, and autonomous merge remain behind later acceptance gates.
