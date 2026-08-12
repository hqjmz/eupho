import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import test from 'node:test';

import { parseRepositoryConfigText } from '../src/config/load.js';
import type { RepositoryConfig } from '../src/config/types.js';
import type { IssueSnapshot, RepositorySnapshot } from '../src/domain/candidate.js';
import { planCandidates } from '../src/domain/planner.js';

const repository: RepositorySnapshot = {
  id: 4242,
  nameWithOwner: 'example/eupho-target',
  defaultBranch: 'main',
  baseSha: '1111111111111111111111111111111111111111',
  policyPath: '.github/eupho.yml',
  policyContent: null,
};

function issue(number: number, labels: string[]): IssueSnapshot {
  return {
    number,
    title: `Issue ${number}`,
    url: `https://github.example/example/eupho-target/issues/${number}`,
    labels,
    updatedAt: '2026-08-12T00:00:00.000Z',
  };
}

async function loadConfig(): Promise<RepositoryConfig> {
  const source = await readFile(resolve(process.cwd(), '.github/eupho.yml'), 'utf8');
  return parseRepositoryConfigText(source, '.github/eupho.yml');
}

test('planning is deterministic, sorted, and capped by repository concurrency', async () => {
  const config = await loadConfig();
  const issues = [
    issue(9, ['agent:ready']),
    issue(5, ['agent:ready', 'agent:wip']),
    issue(1, ['agent:ready', 'agent:risk:docs-only']),
    issue(7, ['agent:ready']),
    issue(2, ['triage']),
  ];

  const first = planCandidates(repository, issues, config);
  const repeatedWithDifferentInputOrder = planCandidates(repository, [...issues].reverse(), config);

  assert.deepEqual(repeatedWithDifferentInputOrder, first);
  assert.deepEqual(
    first.candidates.map((candidate) => candidate.issueNumber),
    [1, 7],
  );
  assert.equal(new Set(first.candidates.map((candidate) => candidate.candidateId)).size, 2);
  assert.deepEqual(
    first.diagnostics.map(({ code, issueNumber }) => ({ code, issueNumber })),
    [
      { code: 'ineligible_state_labels', issueNumber: 2 },
      { code: 'ineligible_state_labels', issueNumber: 5 },
      { code: 'capacity_deferred', issueNumber: 9 },
    ],
  );
  assert.match(first.policyDigest, /^sha256:[a-f0-9]{64}$/);
});

test('an autonomous route selects unattended isolation without changing unrouted defaults', async () => {
  const config = await loadConfig();
  const result = planCandidates(
    repository,
    [issue(2, ['agent:ready']), issue(1, ['agent:ready', 'agent:risk:docs-only'])],
    config,
  );

  const routed = result.candidates.find((candidate) => candidate.issueNumber === 1);
  assert.ok(routed);
  assert.equal(routed.executionMode, 'unattended');
  assert.equal(routed.workspaceType, 'ephemeral_clone');
  assert.equal(routed.mergePolicy, 'autonomous-low-risk');
  assert.equal(routed.routeLabel, 'agent:risk:docs-only');

  const defaulted = result.candidates.find((candidate) => candidate.issueNumber === 2);
  assert.ok(defaulted);
  assert.equal(defaulted.executionMode, 'attended');
  assert.equal(defaulted.workspaceType, 'worktree');
  assert.equal(defaulted.mergePolicy, 'human-final-approval');
  assert.equal(defaulted.routeLabel, null);
});

test('candidate identity binds repository, issue, base revision, and policy', async () => {
  const config = await loadConfig();
  const input = [issue(3, ['agent:ready'])];
  const original = planCandidates(repository, input, config).candidates[0];
  assert.ok(original);

  const repeated = planCandidates(repository, input, structuredClone(config)).candidates[0];
  assert.equal(repeated?.candidateId, original.candidateId);

  const newBase = planCandidates(
    { ...repository, baseSha: '2222222222222222222222222222222222222222' },
    input,
    config,
  ).candidates[0];
  assert.notEqual(newBase?.candidateId, original.candidateId);

  const changedPolicy = structuredClone(config);
  changedPolicy.limits.maxDiffLines += 1;
  const withChangedPolicy = planCandidates(repository, input, changedPolicy).candidates[0];
  assert.notEqual(withChangedPolicy?.candidateId, original.candidateId);
  assert.notEqual(withChangedPolicy?.policyDigest, original.policyDigest);
});

test('ineligible issues never consume the concurrency budget', async () => {
  const config = await loadConfig();
  config.concurrency = 1;
  const result = planCandidates(
    repository,
    [issue(1, ['agent:wip']), issue(2, ['agent:ready']), issue(3, ['agent:ready'])],
    config,
  );

  assert.deepEqual(result.candidates.map((candidate) => candidate.issueNumber), [2]);
  assert.deepEqual(
    result.diagnostics.map(({ code, issueNumber }) => ({ code, issueNumber })),
    [
      { code: 'ineligible_state_labels', issueNumber: 1 },
      { code: 'capacity_deferred', issueNumber: 3 },
    ],
  );
});

test('already-active repository runs consume concurrency before new candidates', async () => {
  const config = await loadConfig();
  config.concurrency = 2;
  const result = planCandidates(
    repository,
    [issue(1, ['agent:ready']), issue(2, ['agent:ready'])],
    config,
    [90],
  );

  assert.deepEqual(result.candidates.map((candidate) => candidate.issueNumber), [1]);
  assert.deepEqual(
    result.diagnostics.map(({ code, issueNumber }) => ({ code, issueNumber })),
    [{ code: 'capacity_deferred', issueNumber: 2 }],
  );
});

test('an issue matching multiple autonomous classes fails closed', async () => {
  const config = await loadConfig();
  config.routing.autonomousClasses.push({
    label: 'agent:risk:test-only',
    executionMode: 'unattended',
    allowedPaths: ['test/**'],
  });
  const result = planCandidates(
    repository,
    [issue(8, ['agent:ready', 'agent:risk:docs-only', 'agent:risk:test-only'])],
    config,
  );

  assert.deepEqual(result.candidates, []);
  assert.deepEqual(result.diagnostics.map((entry) => entry.code), ['ambiguous_autonomous_route']);
});
