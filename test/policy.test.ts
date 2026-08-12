import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import test from 'node:test';

import { resolvePolicy } from '../src/cli/policy.js';
import type { RepositorySnapshot } from '../src/domain/candidate.js';
import { EuphoError } from '../src/errors.js';
import type { GitHubReader } from '../src/github/types.js';

test('trusted policy may select a non-default base only when that base agrees', async () => {
  const sample = await readFile(resolve('.github/eupho.yml'), 'utf8');
  const releasePolicy = sample.replace('base_branch: main', 'base_branch: release');
  const requests: Array<string | undefined> = [];
  const reader = readerFor((baseBranch) => {
    requests.push(baseBranch);
    return snapshot(baseBranch ?? 'main', releasePolicy);
  });

  const resolvedPolicy = await resolvePolicy({
    reader,
    repository: 'acme/widgets',
    cwd: process.cwd(),
  });

  assert.deepEqual(requests, [undefined, 'release']);
  assert.equal(resolvedPolicy.config.baseBranch, 'release');
  assert.equal(resolvedPolicy.trustedBase, true);
});

test('a policy redirect chain fails closed instead of changing trust anchors repeatedly', async () => {
  const sample = await readFile(resolve('.github/eupho.yml'), 'utf8');
  const requests: Array<string | undefined> = [];
  const reader = readerFor((baseBranch) => {
    requests.push(baseBranch);
    const configuredBranch = baseBranch === undefined ? 'release' : 'other';
    return snapshot(baseBranch ?? 'main', sample.replace('base_branch: main', `base_branch: ${configuredBranch}`));
  });

  await assert.rejects(
    resolvePolicy({ reader, repository: 'acme/widgets', cwd: process.cwd() }),
    (error: unknown) => error instanceof EuphoError && error.code === 'unstable_policy_base',
  );
  assert.deepEqual(requests, [undefined, 'release']);
});

function readerFor(
  repository: (baseBranch: string | undefined) => RepositorySnapshot,
): GitHubReader {
  return {
    repository: async (_name, baseBranch) => repository(baseBranch),
    readyIssues: async () => [],
    activeIssueNumbers: async () => [],
    labelExists: async () => true,
    branchPolicy: async () => ({
      strictRequiredChecks: true,
      dismissStaleApprovals: true,
      requiredApprovingReviewCount: 1,
      bypassAppIds: [],
      bypassVerificationComplete: true,
      requiredChecks: [],
      sources: [],
    }),
  };
}

function snapshot(baseBranch: string, policyContent: string): RepositorySnapshot {
  return {
    id: 1,
    nameWithOwner: 'acme/widgets',
    defaultBranch: 'main',
    baseSha: `${baseBranch}-sha`,
    policyPath: '.github/eupho.yml',
    policyContent,
  };
}
