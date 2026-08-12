import assert from 'node:assert/strict';
import { chmod, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';

import { EuphoError } from '../src/errors.js';
import { GhReader } from '../src/github/gh-reader.js';

test('GitHub reader validates and normalizes read-only CLI responses', async () => {
  await withFakeGh(async (binary) => {
    const reader = new GhReader({ binary, environment: { PATH: process.env.PATH } });
    const repository = await reader.repository('acme/widgets');
    assert.equal(repository.id, 77);
    assert.equal(repository.baseSha, 'a'.repeat(40));
    assert.equal(repository.policyPath, '.github/eupho.yml');
    assert.equal(repository.policyContent, 'version: 1\n');

    const issues = await reader.readyIssues('acme/widgets', 'agent:ready');
    assert.deepEqual(issues.map((issue) => issue.number), [7]);
    assert.deepEqual(
      await reader.activeIssueNumbers('acme/widgets', ['agent:wip', 'in-review']),
      [9],
    );
    assert.equal(await reader.labelExists('acme/widgets', 'agent:ready'), true);
    assert.deepEqual(await reader.branchPolicy('acme/widgets', 'main'), {
      strictRequiredChecks: true,
      dismissStaleApprovals: true,
      requiredApprovingReviewCount: 1,
      bypassAppIds: [],
      bypassVerificationComplete: true,
      requiredChecks: [
        { context: 'agent-review', appId: 123456, source: 'classic_protection' },
      ],
      sources: ['classic_protection'],
    });
  });
});

test('GitHub reader fails closed on a malformed external response', async () => {
  await withFakeGh(async (binary) => {
    const reader = new GhReader({
      binary,
      environment: { PATH: process.env.PATH, FAKE_BAD_RESPONSE: '1' },
    });
    await assert.rejects(
      reader.repository('acme/widgets'),
      (error: unknown) =>
        error instanceof EuphoError && error.code === 'invalid_github_response',
    );
  });
});

test('GitHub reader exposes a ruleset bypass by the expected App', async () => {
  await withFakeGh(async (binary) => {
    const reader = new GhReader({
      binary,
      environment: { PATH: process.env.PATH, FAKE_RULESET: '1' },
    });
    const policy = await reader.branchPolicy('acme/widgets', 'main');
    assert.deepEqual(policy.bypassAppIds, [123456]);
    assert.equal(policy.bypassVerificationComplete, true);
    assert.deepEqual(policy.sources, ['classic_protection', 'ruleset']);
  });
});

async function withFakeGh(operation: (binary: string) => Promise<void>): Promise<void> {
  const root = await mkdtemp(join(tmpdir(), 'eupho-fake-gh-'));
  const binary = join(root, 'fake-gh.mjs');
  const source = `#!/usr/bin/env node
const args = process.argv.slice(2);
const send = (value) => process.stdout.write(JSON.stringify(value));
if (process.env.FAKE_BAD_RESPONSE === '1') {
  send({ id: 'not-a-number', full_name: 'acme/widgets', default_branch: 'main' });
  process.exit(0);
}
if (args[0] === 'api') {
  const endpoint = args[3];
  if (endpoint === 'repos/acme/widgets') send({ id: 77, full_name: 'acme/widgets', default_branch: 'main' });
  else if (endpoint.startsWith('repos/acme/widgets/commits/')) send({ sha: '${'a'.repeat(40)}' });
  else if (endpoint === 'repos/acme/widgets/contents/.github/eupho.yml') send({ encoding: 'base64', content: Buffer.from('version: 1\\n').toString('base64') });
  else if (endpoint.startsWith('repos/acme/widgets/labels/')) send({ name: 'agent:ready' });
  else if (endpoint.endsWith('/branches/main/protection')) send({ required_status_checks: { strict: true, checks: [{ context: 'agent-review', app_id: 123456 }] }, required_pull_request_reviews: { dismiss_stale_reviews: true, required_approving_review_count: 1 } });
  else if (endpoint.endsWith('/rules/branches/main')) send(process.env.FAKE_RULESET === '1' ? [{ type: 'required_status_checks', ruleset_id: 42, parameters: { strict_required_status_checks_policy: true, required_status_checks: [{ context: 'agent-review', integration_id: 123456 }] } }] : []);
  else if (endpoint.endsWith('/rulesets/42')) send({ id: 42, bypass_actors: [{ actor_id: 123456, actor_type: 'Integration', bypass_mode: 'always' }] });
  else { process.stderr.write('HTTP 404: Not Found\\n'); process.exit(1); }
} else if (args[0] === 'issue' && args[1] === 'list') {
  const jsonFields = args[args.indexOf('--json') + 1];
  if (jsonFields === 'number') send([{ number: 9 }]);
  else send([{ number: 7, title: 'Safe title', url: 'https://github.com/acme/widgets/issues/7', labels: [{ name: 'agent:ready' }], updatedAt: '2026-08-12T00:00:00.000Z' }]);
} else {
  process.stderr.write('unexpected fake gh arguments: ' + args.join(' ') + '\\n');
  process.exit(2);
}
`;
  try {
    await writeFile(binary, source, { encoding: 'utf8', mode: 0o700 });
    await chmod(binary, 0o700);
    await operation(binary);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}
