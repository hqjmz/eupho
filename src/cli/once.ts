import { loadHostConfig } from '../config/load.js';
import type { CandidateSnapshot } from '../domain/candidate.js';
import { planCandidates } from '../domain/planner.js';
import { EuphoError } from '../errors.js';
import { GhReader } from '../github/gh-reader.js';
import type { GitHubReader } from '../github/types.js';
import { CandidateStore } from '../infra/candidate-store.js';
import { RepositoryLock } from '../infra/repository-lock.js';
import { defaultStateRoot, findGitWorktreeRoot, resolveSafeStateRoot } from '../infra/state-root.js';
import { resolvePolicy } from './policy.js';

export interface OnceReport extends CandidateSnapshot {
  observeOnly: true;
}

export async function once(options: {
  cwd: string;
  repository: string;
  configPath?: string;
  hostConfigPath?: string;
  environment?: NodeJS.ProcessEnv;
  now?: () => Date;
  reader?: GitHubReader;
}): Promise<OnceReport> {
  const environment = options.environment ?? process.env;
  const reader =
    options.reader ??
    new GhReader({
      environment,
      ...(environment.EUPHO_GITHUB_TOKEN === undefined
        ? {}
        : { token: environment.EUPHO_GITHUB_TOKEN }),
    });
  const identityPolicy = await resolvePolicy({
    reader,
    repository: options.repository,
    cwd: options.cwd,
    ...(options.configPath === undefined ? {} : { explicitConfig: options.configPath }),
  });
  const stateRoot = await resolveSafeStateRoot(
    options.hostConfigPath === undefined
      ? defaultStateRoot(environment)
      : (await loadHostConfig(options.hostConfigPath)).value.stateRoot,
    await findGitWorktreeRoot(options.cwd),
  );

  const lock = await RepositoryLock.acquire(stateRoot, identityPolicy.repository.id);
  try {
    const resolvedPolicy = await resolvePolicy({
      reader,
      repository: options.repository,
      cwd: options.cwd,
      ...(options.configPath === undefined ? {} : { explicitConfig: options.configPath }),
    });
    if (
      resolvedPolicy.repository.id !== identityPolicy.repository.id ||
      resolvedPolicy.repository.nameWithOwner !== identityPolicy.repository.nameWithOwner
    ) {
      throw new EuphoError(
        'repository_identity_changed',
        `Repository identity changed while acquiring the lock for ${options.repository}`,
      );
    }
    lock.assertHeld();
    const issueLimit = Math.max(100, resolvedPolicy.config.concurrency * 4);
    const issues = await reader.readyIssues(
      options.repository,
      resolvedPolicy.config.labels.ready,
      issueLimit,
    );
    const activeIssueNumbers = await reader.activeIssueNumbers(
      options.repository,
      [
        resolvedPolicy.config.labels.working,
        resolvedPolicy.config.labels.review,
        resolvedPolicy.config.labels.human,
      ],
      issueLimit,
    );
    const plan = planCandidates(
      resolvedPolicy.repository,
      issues,
      resolvedPolicy.config,
      activeIssueNumbers,
    );
    lock.assertHeld();
    const snapshot: CandidateSnapshot = {
      schemaVersion: 1,
      repositoryId: resolvedPolicy.repository.id,
      repository: resolvedPolicy.repository.nameWithOwner,
      baseSha: resolvedPolicy.repository.baseSha,
      policyDigest: plan.policyDigest,
      policySource: resolvedPolicy.source,
      trustedBase: resolvedPolicy.trustedBase,
      observedAt: (options.now ?? (() => new Date()))().toISOString(),
      candidates: plan.candidates,
      diagnostics: plan.diagnostics,
    };
    lock.assertHeld();
    await new CandidateStore(stateRoot).put(snapshot);
    return {
      ...snapshot,
      observeOnly: true,
    };
  } finally {
    await lock.release();
  }
}
