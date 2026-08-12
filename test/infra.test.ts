import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';

import type { CandidateSnapshot } from '../src/domain/candidate.js';
import { EuphoError } from '../src/errors.js';
import { CandidateStore } from '../src/infra/candidate-store.js';
import { RepositoryLock } from '../src/infra/repository-lock.js';

test('candidate store returns null/empty for unseen repositories and round-trips snapshots', async () => {
  await withTempDirectory('eupho-candidates-', async (stateRoot) => {
    const store = new CandidateStore(stateRoot);
    assert.equal(await store.get(42), null);
    assert.deepEqual(await store.list(), []);

    const snapshot = makeSnapshot(42, 'acme/widgets');
    await store.put(snapshot);

    assert.deepEqual(await store.get(42), snapshot);
    assert.deepEqual(await store.list(), [snapshot]);
  });
});

test('candidate store atomically replaces an existing repository snapshot', async () => {
  await withTempDirectory('eupho-candidates-replace-', async (stateRoot) => {
    const store = new CandidateStore(stateRoot);
    const first = makeSnapshot(7, 'acme/first');
    const second: CandidateSnapshot = {
      ...first,
      baseSha: 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
      observedAt: '2026-08-12T00:01:00.000Z',
      candidates: [],
      diagnostics: [{ code: 'state_conflict', issueNumber: 11, message: 'Conflicting state labels' }],
    };

    await store.put(first);
    await store.put(second);

    assert.deepEqual(await store.get(7), second);
    assert.equal((await store.list()).length, 1);
  });
});

test('candidate store uses private permissions for persisted state', async () => {
  await withTempDirectory('eupho-candidates-mode-', async (stateRoot) => {
    const store = new CandidateStore(stateRoot);
    await store.put(makeSnapshot(5, 'acme/private'));

    const repositoryDirectory = join(stateRoot, 'repositories', '5');
    const candidateFile = join(repositoryDirectory, 'candidates.json');
    assert.equal((await stat(repositoryDirectory)).mode & 0o777, 0o700);
    assert.equal((await stat(candidateFile)).mode & 0o777, 0o600);
  });
});

test('candidate store rejects malformed snapshots before writing', async () => {
  await withTempDirectory('eupho-candidates-invalid-', async (stateRoot) => {
    const store = new CandidateStore(stateRoot);
    const invalid = { ...makeSnapshot(1, 'acme/invalid'), schemaVersion: 2 };

    await assert.rejects(
      store.put(invalid as unknown as CandidateSnapshot),
      (error: unknown) => isEuphoError(error, 'invalid_candidate_state'),
    );
    assert.deepEqual(await store.list(), []);
  });
});

test('repository OS lock is exclusive, records its owner, and can be reacquired after release', async () => {
  await withTempDirectory('eupho-lock-', async (stateRoot) => {
    const lock = await RepositoryLock.acquire(stateRoot, 99);
    const lockPath = join(stateRoot, 'locks', '99.lock');

    try {
      lock.assertHeld();
      const owner = JSON.parse(await readFile(lockPath, 'utf8')) as {
        pid: number;
        holderPid: number;
        acquiredAt: string;
      };
      assert.equal(owner.pid, process.pid);
      assert.equal(Number.isSafeInteger(owner.holderPid), true);
      assert.equal(Number.isNaN(Date.parse(owner.acquiredAt)), false);
      assert.equal((await stat(lockPath)).mode & 0o777, 0o600);

      await assert.rejects(
        RepositoryLock.acquire(stateRoot, 99),
        (error: unknown) =>
          error instanceof EuphoError && error.code === 'repository_locked' && error.exitCode === 2,
      );
    } finally {
      await lock.release();
    }

    // The diagnostic file may be retained or removed by the platform backend;
    // it is never the lock. Immediate reacquisition proves kernel release.
    const reacquired = await RepositoryLock.acquire(stateRoot, 99);
    reacquired.assertHeld();
    await reacquired.release();
    assert.throws(
      () => reacquired.assertHeld(),
      (error: unknown) => isEuphoError(error, 'repository_lock_lost'),
    );
  });
});

function makeSnapshot(repositoryId: number, repository: string): CandidateSnapshot {
  return {
    schemaVersion: 1,
    repositoryId,
    repository,
    baseSha: 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
    policyDigest: `sha256:${'0'.repeat(64)}`,
    policySource: 'github:acme/widgets/.github/eupho.yml@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
    trustedBase: true,
    observedAt: '2026-08-12T00:00:00.000Z',
    candidates: [
      {
        candidateId: `candidate-${repositoryId.toString(16).padStart(20, '0')}`,
        action: 'would_claim',
        repositoryId,
        repository,
        issueNumber: 11,
        issueTitle: 'Exercise the control plane',
        issueUrl: `https://github.com/${repository}/issues/11`,
        baseSha: 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
        policyDigest: `sha256:${'0'.repeat(64)}`,
        executionMode: 'attended',
        workspaceType: 'worktree',
        mergePolicy: 'human-final-approval',
        routeLabel: null,
        preconditions: ['repository_lock'],
      },
    ],
    diagnostics: [],
  };
}

function isEuphoError(error: unknown, code: string): boolean {
  return error instanceof EuphoError && error.code === code;
}

async function withTempDirectory(
  prefix: string,
  operation: (path: string) => Promise<void>,
): Promise<void> {
  const path = await mkdtemp(join(tmpdir(), prefix));
  try {
    await operation(path);
  } finally {
    await rm(path, { recursive: true, force: true });
  }
}
