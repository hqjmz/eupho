import { createHash } from 'node:crypto';

import type { RepositoryConfig } from '../config/types.js';
import { canonicalJson } from '../security/canonical-json.js';
import type {
  CandidatePlan,
  IssueSnapshot,
  PlanningDiagnostic,
  RepositorySnapshot,
} from './candidate.js';
import { stableCandidateId } from './candidate.js';
import type { ExecutionMode, MergePolicy, WorkspaceType } from './run.js';

export interface PlanResult {
  candidates: CandidatePlan[];
  diagnostics: PlanningDiagnostic[];
  policyDigest: string;
}

export function planCandidates(
  repository: RepositorySnapshot,
  issues: IssueSnapshot[],
  config: RepositoryConfig,
  activeIssueNumbers: readonly number[] = [],
): PlanResult {
  const policyDigest = `sha256:${createHash('sha256').update(canonicalJson(config)).digest('hex')}`;
  const workflowLabels = new Set([
    config.labels.ready,
    config.labels.working,
    config.labels.review,
    config.labels.human,
  ]);
  const diagnostics: PlanningDiagnostic[] = [];
  const eligible: CandidatePlan[] = [];

  for (const issue of [...issues].sort((left, right) => left.number - right.number)) {
    const activeWorkflowLabels = issue.labels.filter((label) => workflowLabels.has(label));
    if (activeWorkflowLabels.length !== 1 || activeWorkflowLabels[0] !== config.labels.ready) {
      diagnostics.push({
        code: 'ineligible_state_labels',
        issueNumber: issue.number,
        message: `Expected only ${config.labels.ready}; found ${activeWorkflowLabels.join(', ') || 'none'}`,
      });
      continue;
    }

    const matchingRoutes = config.routing.autonomousClasses.filter((entry) =>
      issue.labels.includes(entry.label),
    );
    if (matchingRoutes.length > 1) {
      diagnostics.push({
        code: 'ambiguous_autonomous_route',
        issueNumber: issue.number,
        message: `Issue matches multiple autonomous classes: ${matchingRoutes
          .map((entry) => entry.label)
          .join(', ')}`,
      });
      continue;
    }
    const route = matchingRoutes[0];
    const executionMode: ExecutionMode = route?.executionMode ?? config.execution.defaultMode;
    const workspaceType: WorkspaceType =
      executionMode === 'unattended'
        ? config.execution.unattended.workspace
        : config.execution.attended.workspace;
    const mergePolicy: MergePolicy = route === undefined ? config.mergePolicy : 'autonomous-low-risk';

    eligible.push({
      candidateId: stableCandidateId({
        repositoryId: repository.id,
        issueNumber: issue.number,
        baseSha: repository.baseSha,
        policyDigest,
      }),
      action: 'would_claim',
      repositoryId: repository.id,
      repository: repository.nameWithOwner,
      issueNumber: issue.number,
      issueTitle: issue.title,
      issueUrl: issue.url,
      baseSha: repository.baseSha,
      policyDigest,
      executionMode,
      workspaceType,
      mergePolicy,
      routeLabel: route?.label ?? null,
      preconditions: [
        `issue remains open with only ${config.labels.ready}`,
        `base remains ${repository.baseSha}`,
        'repository capacity remains available',
      ],
    });
  }

  const activeCount = new Set(activeIssueNumbers).size;
  const availableCapacity = Math.max(0, config.concurrency - activeCount);
  const candidates = eligible.slice(0, availableCapacity);
  for (const deferred of eligible.slice(availableCapacity)) {
    diagnostics.push({
      code: 'capacity_deferred',
      issueNumber: deferred.issueNumber,
      message: `Deferred by concurrency limit ${config.concurrency}; ${activeCount} active run(s) observed`,
    });
  }

  return { candidates, diagnostics, policyDigest };
}
