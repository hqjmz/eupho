import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';

import { EuphoError } from '../src/errors.js';
import {
  assertSafeStateRoot,
  defaultStateRoot,
  findGitWorktreeRoot,
  pathsOverlap,
  resolveSafeStateRoot,
} from '../src/infra/state-root.js';

test('state root normalizes safely and never defaults inside the repository', () => {
  const root = defaultStateRoot({ XDG_STATE_HOME: '/tmp/eupho-user-state' });
  assert.equal(root, '/tmp/eupho-user-state/eupho');
  assert.equal(pathsOverlap(process.cwd(), root), false);
});

test('state root rejects filesystem roots and repository-owned locations', () => {
  for (const root of ['/', '/tmp/..']) {
    assert.throws(
      () => assertSafeStateRoot(root),
      (error: unknown) => error instanceof EuphoError && error.code === 'unsafe_state_root',
    );
  }
  assert.throws(
    () => assertSafeStateRoot(`${process.cwd()}/.eupho`, process.cwd()),
    (error: unknown) => error instanceof EuphoError && error.code === 'unsafe_state_root',
  );
});

test('path overlap is component-aware', () => {
  assert.equal(pathsOverlap('/tmp/eupho/state', '/tmp/eupho/state/workspaces'), true);
  assert.equal(pathsOverlap('/tmp/eupho/state', '/tmp/eupho/state-other'), false);
});

test('state-root resolution canonicalizes an existing ancestor before writes', async () => {
  const resolved = await resolveSafeStateRoot('/tmp/eupho-state-root-test/new-state');
  assert.match(resolved, /\/eupho-state-root-test\/new-state$/);
  assert.notEqual(resolved, '/');
});

test('Git worktree discovery does not mistake an arbitrary current directory for a repository', async () => {
  assert.equal(await findGitWorktreeRoot('/tmp'), undefined);
  const root = await mkdtemp(join(tmpdir(), 'eupho-git-root-'));
  try {
    await mkdir(join(root, '.git'));
    await mkdir(join(root, 'nested'));
    assert.equal(await findGitWorktreeRoot(join(root, 'nested')), root);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
