import { lstat, realpath } from 'node:fs/promises';
import { homedir } from 'node:os';
import { basename, dirname, isAbsolute, join, normalize, parse, relative, resolve } from 'node:path';

import { EuphoError } from '../errors.js';

export function defaultStateRoot(environment: NodeJS.ProcessEnv = process.env): string {
  const xdgStateHome = environment.XDG_STATE_HOME;
  const parent =
    xdgStateHome !== undefined && isAbsolute(xdgStateHome)
      ? normalize(xdgStateHome)
      : process.platform === 'darwin'
        ? join(homedir(), 'Library', 'Application Support')
        : join(homedir(), '.local', 'state');
  return assertSafeStateRoot(join(parent, 'eupho'));
}

export async function findGitWorktreeRoot(start: string): Promise<string | undefined> {
  let candidate = resolve(start);
  while (true) {
    try {
      await lstat(join(candidate, '.git'));
      return candidate;
    } catch (error) {
      if (!hasErrorCode(error, 'ENOENT')) throw error;
    }
    const parent = dirname(candidate);
    if (parent === candidate) return undefined;
    candidate = parent;
  }
}

export function assertSafeStateRoot(path: string, repositoryRoot?: string): string {
  const normalized = resolve(path);
  if (normalized === parse(normalized).root) {
    throw new EuphoError('unsafe_state_root', `State root cannot resolve to ${normalized}`);
  }
  if (repositoryRoot !== undefined && containsPath(resolve(repositoryRoot), normalized)) {
    throw new EuphoError(
      'unsafe_state_root',
      `State root ${normalized} must be outside the working repository ${resolve(repositoryRoot)}`,
    );
  }
  return normalized;
}

export async function resolveSafeStateRoot(path: string, repositoryRoot?: string): Promise<string> {
  const lexical = assertSafeStateRoot(path, repositoryRoot);
  let existingAncestor = lexical;
  const missingSegments: string[] = [];

  while (true) {
    try {
      const information = await lstat(existingAncestor);
      if (existingAncestor === lexical && information.isSymbolicLink()) {
        throw new EuphoError(
          'unsafe_state_root',
          `State root ${lexical} must not itself be a symbolic link`,
        );
      }
      if (!information.isDirectory() && !information.isSymbolicLink()) {
        throw new EuphoError(
          'unsafe_state_root',
          `State root ancestor ${existingAncestor} is not a directory`,
        );
      }
      break;
    } catch (error) {
      if (!hasErrorCode(error, 'ENOENT')) throw error;
      const parent = dirname(existingAncestor);
      if (parent === existingAncestor) throw error;
      missingSegments.unshift(basename(existingAncestor));
      existingAncestor = parent;
    }
  }

  const canonicalAncestor = await realpath(existingAncestor);
  if (
    canonicalAncestor === parse(canonicalAncestor).root &&
    existingAncestor !== parse(existingAncestor).root
  ) {
    throw new EuphoError(
      'unsafe_state_root',
      `State root ancestor ${existingAncestor} resolves to the filesystem root`,
    );
  }
  return assertSafeStateRoot(resolve(canonicalAncestor, ...missingSegments), repositoryRoot);
}

export function pathsOverlap(left: string, right: string): boolean {
  const normalizedLeft = resolve(left);
  const normalizedRight = resolve(right);
  return containsPath(normalizedLeft, normalizedRight) || containsPath(normalizedRight, normalizedLeft);
}

function containsPath(parent: string, candidate: string): boolean {
  const child = relative(parent, candidate);
  return child === '' || (!child.startsWith('..') && !isAbsolute(child));
}

function hasErrorCode(error: unknown, code: string): boolean {
  return (
    error instanceof Error &&
    'code' in error &&
    (error as NodeJS.ErrnoException).code === code
  );
}
