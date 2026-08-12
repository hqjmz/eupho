import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import test from 'node:test';

import { once } from '../src/cli/once.js';
import type { RepositorySnapshot } from '../src/domain/candidate.js';
import type { GitHubReader } from '../src/github/types.js';

test('observe-only pass re-reads mutable GitHub state under the repository lock', async () => {
  const root = await mkdtemp(join(tmpdir(), 'eupho-once-'));
  const policy = await readFile(resolve('.github/eupho.yml'), 'utf8');
  const calls: string[] = [];
  let repositoryRead = 0;
  const reader: GitHubReader = {
    repository: async () => {
      repositoryRead += 1;
      calls.push(`repository:${repositoryRead}`);
      return repositorySnapshot(repositoryRead === 1 ? 'a'.repeat(40) : 'b'.repeat(40), policy);
    },
    readyIssues: async () => {
      calls.push('ready');
      return [
        {
          number: 7,
          title: 'Observed after lock',
          url: 'https://github.com/acme/widgets/issues/7',
          labels: ['agent:ready'],
          updatedAt: '2026-08-12T00:00:00.000Z',
        },
      ];
    },
    activeIssueNumbers: async () => {
      calls.push('active');
      return [];
    },
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

  try {
    const report = await once({
      cwd: '/tmp',
      repository: 'acme/widgets',
      environment: { XDG_STATE_HOME: join(root, 'state') },
      now: () => new Date('2026-08-12T01:00:00.000Z'),
      reader,
    });

    assert.deepEqual(calls, ['repository:1', 'repository:2', 'ready', 'active']);
    assert.equal(report.baseSha, 'b'.repeat(40));
    assert.equal(report.trustedBase, true);
    const stored = JSON.parse(
      await readFile(join(root, 'state', 'eupho', 'repositories', '77', 'candidates.json'), 'utf8'),
    ) as { baseSha: string; trustedBase: boolean };
    assert.equal(stored.baseSha, 'b'.repeat(40));
    assert.equal(stored.trustedBase, true);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

function repositorySnapshot(baseSha: string, policyContent: string): RepositorySnapshot {
  return {
    id: 77,
    nameWithOwner: 'acme/widgets',
    defaultBranch: 'main',
    baseSha,
    policyPath: '.github/eupho.yml',
    policyContent,
  };
}
