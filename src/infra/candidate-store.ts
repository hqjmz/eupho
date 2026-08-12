import { readdir } from 'node:fs/promises';
import { join } from 'node:path';

import type { CandidateSnapshot } from '../domain/candidate.js';
import { EuphoError } from '../errors.js';
import { atomicWriteJson, readJsonFile } from './atomic-file.js';

export class CandidateStore {
  constructor(private readonly stateRoot: string) {}

  async put(snapshot: CandidateSnapshot): Promise<void> {
    assertCandidateSnapshot(snapshot);
    await atomicWriteJson(this.pathFor(snapshot.repositoryId), snapshot);
  }

  async get(repositoryId: number): Promise<CandidateSnapshot | null> {
    try {
      const snapshot = await readJsonFile<CandidateSnapshot>(this.pathFor(repositoryId));
      assertCandidateSnapshot(snapshot);
      return snapshot;
    } catch (error) {
      if (error instanceof EuphoError && error.cause instanceof Error && 'code' in error.cause) {
        if ((error.cause as NodeJS.ErrnoException).code === 'ENOENT') return null;
      }
      throw error;
    }
  }

  async list(): Promise<CandidateSnapshot[]> {
    const root = join(this.stateRoot, 'repositories');
    let entries: string[];
    try {
      entries = await readdir(root);
    } catch (error) {
      if (error instanceof Error && 'code' in error && (error as NodeJS.ErrnoException).code === 'ENOENT') {
        return [];
      }
      throw error;
    }

    const snapshots: CandidateSnapshot[] = [];
    for (const entry of entries.sort((left, right) => Number(left) - Number(right))) {
      if (!/^\d+$/.test(entry)) continue;
      const snapshot = await this.get(Number(entry));
      if (snapshot !== null) snapshots.push(snapshot);
    }
    return snapshots;
  }

  private pathFor(repositoryId: number): string {
    return join(this.stateRoot, 'repositories', String(repositoryId), 'candidates.json');
  }
}

function assertCandidateSnapshot(value: unknown): asserts value is CandidateSnapshot {
  if (!isRecord(value)) invalidState();
  if (
    value.schemaVersion !== 1 ||
    !isPositiveInteger(value.repositoryId) ||
    !isRepository(value.repository) ||
    !isCommitSha(value.baseSha) ||
    !isDigest(value.policyDigest) ||
    typeof value.policySource !== 'string' ||
    value.policySource.length === 0 ||
    typeof value.trustedBase !== 'boolean' ||
    !isIsoInstant(value.observedAt) ||
    !Array.isArray(value.candidates) ||
    !Array.isArray(value.diagnostics)
  ) {
    invalidState();
  }

  for (const candidate of value.candidates) {
    if (
      !isRecord(candidate) ||
      typeof candidate.candidateId !== 'string' ||
      !/^candidate-[a-f0-9]{20}$/.test(candidate.candidateId) ||
      candidate.action !== 'would_claim' ||
      candidate.repositoryId !== value.repositoryId ||
      candidate.repository !== value.repository ||
      !isPositiveInteger(candidate.issueNumber) ||
      typeof candidate.issueTitle !== 'string' ||
      typeof candidate.issueUrl !== 'string' ||
      candidate.baseSha !== value.baseSha ||
      candidate.policyDigest !== value.policyDigest ||
      (candidate.executionMode !== 'attended' && candidate.executionMode !== 'unattended') ||
      (candidate.workspaceType !== 'worktree' && candidate.workspaceType !== 'ephemeral_clone') ||
      (candidate.executionMode === 'attended' && candidate.workspaceType !== 'worktree') ||
      (candidate.executionMode === 'unattended' && candidate.workspaceType !== 'ephemeral_clone') ||
      !['autonomous-low-risk', 'human-final-approval', 'suggest-only'].includes(
        String(candidate.mergePolicy),
      ) ||
      (candidate.routeLabel !== null && typeof candidate.routeLabel !== 'string') ||
      !isStringArray(candidate.preconditions)
    ) {
      invalidState();
    }
  }

  for (const diagnostic of value.diagnostics) {
    if (
      !isRecord(diagnostic) ||
      typeof diagnostic.code !== 'string' ||
      !isPositiveInteger(diagnostic.issueNumber) ||
      typeof diagnostic.message !== 'string'
    ) {
      invalidState();
    }
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function isPositiveInteger(value: unknown): value is number {
  return Number.isSafeInteger(value) && Number(value) > 0;
}

function isRepository(value: unknown): value is string {
  return typeof value === 'string' && /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(value);
}

function isCommitSha(value: unknown): value is string {
  return typeof value === 'string' && /^(?:[a-f0-9]{40}|[a-f0-9]{64})$/i.test(value);
}

function isDigest(value: unknown): value is string {
  return typeof value === 'string' && /^sha256:[a-f0-9]{64}$/i.test(value);
}

function isIsoInstant(value: unknown): value is string {
  if (typeof value !== 'string') return false;
  const timestamp = Date.parse(value);
  return Number.isFinite(timestamp) && new Date(timestamp).toISOString() === value;
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((entry) => typeof entry === 'string');
}

function invalidState(): never {
  throw new EuphoError('invalid_candidate_state', 'Candidate state is malformed');
}
