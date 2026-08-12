import { createHash } from 'node:crypto';

import type { ExecutionMode, MergePolicy, WorkspaceType } from './run.js';

export interface IssueSnapshot {
  number: number;
  title: string;
  url: string;
  labels: string[];
  updatedAt: string;
}

export interface RepositorySnapshot {
  id: number;
  nameWithOwner: string;
  defaultBranch: string;
  baseSha: string;
  policyPath: string | null;
  policyContent: string | null;
}

export interface RepositoryCapacitySnapshot {
  activeIssueNumbers: number[];
}

export interface CandidatePlan {
  candidateId: string;
  action: 'would_claim';
  repositoryId: number;
  repository: string;
  issueNumber: number;
  issueTitle: string;
  issueUrl: string;
  baseSha: string;
  policyDigest: string;
  executionMode: ExecutionMode;
  workspaceType: WorkspaceType;
  mergePolicy: MergePolicy;
  routeLabel: string | null;
  preconditions: string[];
}

export interface PlanningDiagnostic {
  code: string;
  issueNumber: number;
  message: string;
}

export interface CandidateSnapshot {
  schemaVersion: 1;
  repositoryId: number;
  repository: string;
  baseSha: string;
  policyDigest: string;
  policySource: string;
  trustedBase: boolean;
  observedAt: string;
  candidates: CandidatePlan[];
  diagnostics: PlanningDiagnostic[];
}

export function stableCandidateId(input: {
  repositoryId: number;
  issueNumber: number;
  baseSha: string;
  policyDigest: string;
}): string {
  const digest = createHash('sha256')
    .update(
      `${input.repositoryId}\0${input.issueNumber}\0${input.baseSha}\0${input.policyDigest}`,
      'utf8',
    )
    .digest('hex');
  return `candidate-${digest.slice(0, 20)}`;
}
