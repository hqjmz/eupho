import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { chmod, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

import { EuphoError, messageOf } from '../errors.js';
import { ensurePrivateDirectory } from './atomic-file.js';

const LOCKED_MARKER = 'EUPHO_LOCKED\n';
const HOLDER_SOURCE =
  "process.stdout.write('EUPHO_LOCKED\\n');process.stdin.resume();process.stdin.on('end',()=>process.exit(0));";

/**
 * A repository-scoped advisory lock held by a child process. The operating
 * system releases it if either Eupho or the holder crashes; the on-disk file is
 * only diagnostic metadata and is never treated as the lock itself.
 */
export class RepositoryLock {
  private released = false;

  private constructor(
    private readonly path: string,
    private readonly holder: ChildProcessWithoutNullStreams,
  ) {}

  static async acquire(stateRoot: string, repositoryId: number): Promise<RepositoryLock> {
    if (!Number.isSafeInteger(repositoryId) || repositoryId <= 0) {
      throw new EuphoError('invalid_repository_id', `Invalid repository ID ${repositoryId}`);
    }
    const directory = join(stateRoot, 'locks');
    await ensurePrivateDirectory(directory);
    const path = join(directory, `${repositoryId}.lock`);
    const holder = spawnLockHolder(path);

    try {
      await waitForLock(holder, repositoryId, path);
      await writeFile(
        path,
        `${JSON.stringify({ pid: process.pid, holderPid: holder.pid, acquiredAt: new Date().toISOString() })}\n`,
        { encoding: 'utf8', mode: 0o600 },
      );
      await chmod(path, 0o600);
      unrefHolder(holder);
      return new RepositoryLock(path, holder);
    } catch (error) {
      holder.kill('SIGTERM');
      if (error instanceof EuphoError) throw error;
      throw new EuphoError(
        'repository_locked',
        `Repository ${repositoryId} could not acquire the OS lock at ${path}: ${messageOf(error)}`,
        2,
        { cause: error },
      );
    }
  }

  assertHeld(): void {
    if (this.released || this.holder.exitCode !== null || this.holder.signalCode !== null) {
      throw new EuphoError('repository_lock_lost', `OS lock at ${this.path} is no longer held`);
    }
  }

  async release(): Promise<void> {
    if (this.released) return;
    this.released = true;
    const exit = waitForExit(this.holder);
    this.holder.stdin.end();
    const code = await exit;
    if (code !== 0) {
      throw new EuphoError(
        'lock_release_failed',
        `OS lock holder for ${this.path} exited with ${String(code)}`,
      );
    }
  }
}

function spawnLockHolder(path: string): ChildProcessWithoutNullStreams {
  if (process.platform === 'darwin' || process.platform.endsWith('bsd')) {
    return spawn('/usr/bin/lockf', ['-t', '0', path, process.execPath, '-e', HOLDER_SOURCE], {
      stdio: ['pipe', 'pipe', 'pipe'],
    });
  }
  if (process.platform === 'linux') {
    return spawn('flock', ['-n', path, process.execPath, '-e', HOLDER_SOURCE], {
      stdio: ['pipe', 'pipe', 'pipe'],
    });
  }
  throw new EuphoError(
    'unsupported_lock_backend',
    `No Eupho OS-lock backend is available for ${process.platform}`,
  );
}

async function waitForLock(
  holder: ChildProcessWithoutNullStreams,
  repositoryId: number,
  path: string,
): Promise<void> {
  await new Promise<void>((resolve, reject) => {
    let stdout = '';
    let stderr = '';
    let settled = false;
    const timeout = setTimeout(() => {
      finish(
        new EuphoError(
          'lock_start_timeout',
          `Timed out starting the OS lock holder for repository ${repositoryId}`,
          2,
        ),
      );
    }, 5_000);

    const finish = (error?: Error): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      holder.stdout.off('data', onStdout);
      holder.stderr.off('data', onStderr);
      holder.off('error', onError);
      holder.off('exit', onExit);
      if (error === undefined) resolve();
      else reject(error);
    };
    const onStdout = (chunk: Buffer): void => {
      stdout += chunk.toString('utf8');
      if (stdout.includes(LOCKED_MARKER)) finish();
    };
    const onStderr = (chunk: Buffer): void => {
      stderr += chunk.toString('utf8');
    };
    const onError = (error: Error): void => finish(error);
    const onExit = (code: number | null): void =>
      finish(
        new EuphoError(
          'repository_locked',
          `Repository ${repositoryId} is already locked at ${path}${
            stderr.trim().length === 0 ? '' : `: ${stderr.trim()}`
          } (holder exit ${String(code)})`,
          2,
        ),
      );

    holder.stdout.on('data', onStdout);
    holder.stderr.on('data', onStderr);
    holder.once('error', onError);
    holder.once('exit', onExit);
  });
}

function waitForExit(holder: ChildProcessWithoutNullStreams): Promise<number | null> {
  return new Promise((resolve, reject) => {
    if (holder.exitCode !== null) {
      resolve(holder.exitCode);
      return;
    }
    if (holder.signalCode !== null) {
      resolve(1);
      return;
    }
    holder.once('exit', resolve);
    holder.once('error', reject);
  });
}

function unrefHolder(holder: ChildProcessWithoutNullStreams): void {
  holder.unref();
  unrefStream(holder.stdin);
  unrefStream(holder.stdout);
  unrefStream(holder.stderr);
}

function unrefStream(stream: unknown): void {
  if (
    stream !== null &&
    typeof stream === 'object' &&
    'unref' in stream &&
    typeof (stream as { unref?: unknown }).unref === 'function'
  ) {
    (stream as { unref: () => void }).unref();
  }
}
