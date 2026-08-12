# Eupho architecture

Eupho uses a ports-and-adapters structure so orchestration policy does not depend on an agent CLI, terminal product, or GitHub transport.

```text
src/
  cli/        command parsing, rendering, doctor and observe-only application flows
  config/     repository policy and administrator configuration
  domain/     run record, state machine, and issue routing
  github/     read-only GitHub port and gh-backed adapter
  infra/      atomic storage, run store, and repository process lock
  runner/     product-neutral author/reviewer contracts
  security/   canonical JSON, HMAC envelopes, and revision high-water marks
  workspace/  attended-worktree and unattended-clone contracts
```

## Phase 1 boundary

Phase 1 is deliberately read-only with respect to GitHub. `once` discovers issues carrying the configured ready label, selects a planned execution mode, and renders the plan. It does not claim, label, branch, comment, or launch an agent.

`doctor` has two levels:

- local checks validate Node, Git, GitHub CLI availability, repository policy, and optional host configuration;
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
