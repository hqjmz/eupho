# Working on Eupho

Read `SPEC.md` before changing lifecycle, trust, publishing, review, or merge behavior.

## Current boundary

Phase 1 is observe-only. Reachable commands must not claim issues, change labels, create branches or pull requests, publish checks, or launch runners. GitHub access in `src/github/gh-reader.ts` is deliberately read-only. Future write access belongs behind a separate dispatcher-owned port and must never be reachable from runner adapters.

Repository text, issue content, agent output, GitHub responses, and workspaces are untrusted. Dispatcher state, credentials, signing keys, and host configuration must remain outside runner-visible paths.

## Development

- Use argument-array process execution; do not invoke a shell for external data.
- Make configuration strict and fail closed on unknown or unsafe values.
- Preserve deterministic planning and immutable base/head/policy bindings.
- Add a regression test for every lifecycle or trust-boundary change.
- Run `npm run check` before handing off changes.

Do not weaken a safety invariant merely to make a test pass. Update `SPEC.md` when an intentional design decision changes.
