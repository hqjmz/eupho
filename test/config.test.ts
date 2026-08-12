import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import test from 'node:test';

import {
  findRepositoryConfig,
  loadHostConfig,
  loadRepositoryConfig,
  parseHostConfigText,
  parseRepositoryConfigText,
} from '../src/config/load.js';
import { EuphoError } from '../src/errors.js';

const repositoryPolicyPath = resolve(process.cwd(), '.github/eupho.yml');
const hostConfigPath = resolve(process.cwd(), 'config/examples/host.yml');

async function repositoryPolicyText(): Promise<string> {
  return readFile(repositoryPolicyPath, 'utf8');
}

function assertInvalidConfig(error: unknown, expectedMessage: RegExp): boolean {
  assert.ok(error instanceof EuphoError);
  assert.equal(error.code, 'invalid_config');
  assert.match(error.message, expectedMessage);
  return true;
}

test('checked-in repository policy compiles to the strict domain shape', async () => {
  const loaded = await loadRepositoryConfig(repositoryPolicyPath);

  assert.equal(loaded.source, repositoryPolicyPath);
  assert.equal(loaded.value.version, 1);
  assert.equal(loaded.value.baseBranch, 'main');
  assert.equal(loaded.value.concurrency, 2);
  assert.deepEqual(loaded.value.execution, {
    defaultMode: 'attended',
    attended: {
      workspace: 'worktree',
      nativePermissionPrompts: true,
      sandboxProfile: 'optional',
    },
    unattended: {
      workspace: 'ephemeral_clone',
      nativePermissionPrompts: false,
      sandboxProfile: 'hardened-container',
    },
  });
  assert.deepEqual(loaded.value.routing.autonomousClasses, [
    {
      label: 'agent:risk:docs-only',
      executionMode: 'unattended',
      allowedPaths: ['docs/**', '**/*.md'],
    },
  ]);
  assert.ok(loaded.value.review.alwaysBlockingCategories.includes('weakened_or_deleted_tests'));
  assert.equal(loaded.value.limits.modelCostUsdPerRun, '8.00');
  assert.equal(loaded.value.branches.mergeQueue, false);
});

test('repository policy discovery prefers the Eupho path and honors an explicit path', async () => {
  assert.equal(await findRepositoryConfig(process.cwd()), repositoryPolicyPath);
  assert.equal(
    await findRepositoryConfig(process.cwd(), 'config/examples/host.yml'),
    hostConfigPath,
  );
});

test('repository policy rejects unknown fields instead of silently ignoring them', async () => {
  const policy = await repositoryPolicyText();
  const withUnknownField = policy.replace(
    'concurrency: 2',
    'concurrency: 2\nexperimental_shortcut: true',
  );

  assert.throws(
    () => parseRepositoryConfigText(withUnknownField, 'unknown-field.yml'),
    (error: unknown) => assertInvalidConfig(error, /contains unknown field: experimental_shortcut/),
  );
});

test('repository policy rejects ambiguous workflow labels', async () => {
  const policy = await repositoryPolicyText();
  const withDuplicateLabel = policy.replace('working: agent:wip', 'working: agent:ready');

  assert.throws(
    () => parseRepositoryConfigText(withDuplicateLabel, 'duplicate-label.yml'),
    (error: unknown) => assertInvalidConfig(error, /workflow-state labels must be distinct/),
  );
});

test('repository policy enforces the attended and unattended safety contract', async () => {
  const policy = await repositoryPolicyText();
  const unattendedPromptsEnabled = policy.replace(
    /unattended:\n    workspace: ephemeral_clone\n    native_permission_prompts: false/,
    'unattended:\n    workspace: ephemeral_clone\n    native_permission_prompts: true',
  );

  assert.throws(
    () => parseRepositoryConfigText(unattendedPromptsEnabled, 'unsafe-unattended.yml'),
    (error: unknown) =>
      assertInvalidConfig(
        error,
        /execution\.unattended\.native_permission_prompts must be false/,
      ),
  );
});

test('repository policy enforces merge and review invariants', async () => {
  const policy = await repositoryPolicyText();

  assert.throws(
    () =>
      parseRepositoryConfigText(
        policy.replace('always_blocking_categories:\n    - weakened_or_deleted_tests', 'always_blocking_categories:\n    - security'),
        'missing-test-integrity.yml',
      ),
    (error: unknown) => assertInvalidConfig(error, /must include weakened_or_deleted_tests/),
  );
  assert.throws(
    () =>
      parseRepositoryConfigText(
        policy.replace('dismiss_stale_approvals: true', 'dismiss_stale_approvals: false'),
        'stale-approval.yml',
      ),
    (error: unknown) => assertInvalidConfig(error, /must be true for human-final-approval/),
  );
  assert.throws(
    () =>
      parseRepositoryConfigText(
        policy.replace('required_check_source: eupho', 'required_check_source: another-app'),
        'check-source.yml',
      ),
    (error: unknown) => assertInvalidConfig(error, /must match github_app\.slug/),
  );
});

test('checked-in host example compiles and host paths must be absolute', async () => {
  const loaded = await loadHostConfig(hostConfigPath);
  assert.equal(loaded.value.githubApp.appId, 123456);
  assert.equal(loaded.value.sandboxProfiles['hardened-container']?.sharedGitAdmin, false);
  assert.deepEqual(loaded.value.workspaceProfiles['ephemeral_clone'], {
    sharedObjects: false,
    authenticatedRemote: false,
  });
  assert.deepEqual(loaded.value.notificationSinks['local-desktop']?.argv, [
    '/absolute/admin-owned/path/eupho/notify',
  ]);

  const source = await readFile(hostConfigPath, 'utf8');
  const relativeStateRoot = source.replace(
    'state_root: /absolute/admin-owned/path/eupho/state',
    'state_root: relative/state',
  );
  assert.throws(
    () => parseHostConfigText(relativeStateRoot, 'relative-host.yml'),
    (error: unknown) => assertInvalidConfig(error, /state_root must be an absolute path/),
  );

  const rootTraversal = source.replace(
    'state_root: /absolute/admin-owned/path/eupho/state',
    'state_root: /tmp/..',
  );
  assert.throws(
    () => parseHostConfigText(rootTraversal, 'root-host.yml'),
    (error: unknown) => assertInvalidConfig(error, /must not resolve to the filesystem root/),
  );

  const overlappingRoots = source.replace(
    'workspace_root: /absolute/admin-owned/path/eupho/workspaces',
    'workspace_root: /absolute/admin-owned/path/eupho/state/workspaces',
  );
  assert.throws(
    () => parseHostConfigText(overlappingRoots, 'overlap-host.yml'),
    (error: unknown) => assertInvalidConfig(error, /must not overlap workspace_root/),
  );

  const unsafeSandbox = source.replace('network: deny_by_default', 'network: allow_all');
  assert.throws(
    () => parseHostConfigText(unsafeSandbox, 'unsafe-sandbox.yml'),
    (error: unknown) => assertInvalidConfig(error, /network must be one of deny_by_default/),
  );
});

test('autonomous merge is route-scoped and cannot be a repository-wide default', async () => {
  const policy = (await repositoryPolicyText()).replace(
    'merge_policy: human-final-approval',
    'merge_policy: autonomous-low-risk',
  );
  assert.throws(
    () => parseRepositoryConfigText(policy, 'global-autonomous.yml'),
    (error: unknown) => assertInvalidConfig(error, /use an explicit routing\.autonomous_classes/),
  );

  const workflowLabelRoute = (await repositoryPolicyText()).replace(
    'label: agent:risk:docs-only',
    'label: agent:ready',
  );
  assert.throws(
    () => parseRepositoryConfigText(workflowLabelRoute, 'workflow-route.yml'),
    (error: unknown) => assertInvalidConfig(error, /must not be a workflow-state label/),
  );
});
