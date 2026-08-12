export const RUN_STATES = [
  'ready',
  'work_in_progress',
  'in_review',
  'needs_human',
  'merged',
  'completed_no_change',
  'cancelled',
] as const;

export type RunState = (typeof RUN_STATES)[number];

export const RUN_PHASES = [
  'intake',
  'author',
  'validation',
  'review',
  'repair',
  'awaiting_approval',
  'merge_wait',
  'paused',
  'complete',
] as const;

export type RunPhase = (typeof RUN_PHASES)[number];
export type ExecutionMode = 'attended' | 'unattended';
export type WorkspaceType = 'worktree' | 'ephemeral_clone';
export type MergePolicy = 'autonomous-low-risk' | 'human-final-approval' | 'suggest-only';
export type AttentionCode = 'no_change' | 'escalation' | 'external_change';

export interface ReviewBinding {
  baseSha: string;
  headSha: string;
  diffHash: string;
}

export interface RunAttempts {
  author: number;
  validation: number;
  review: number;
  repair: number;
}

export interface RunUsage {
  modelTokens: number;
  costUsd: string;
}

export interface RunRecord {
  schemaVersion: 1;
  revision: number;
  runId: string;
  repositoryId: number;
  repository: string;
  issueNumber: number;
  state: RunState;
  phase: RunPhase;
  executionMode: ExecutionMode;
  workspaceType: WorkspaceType;
  mergePolicy: MergePolicy;
  baseBranch: string;
  baseSha: string;
  branch: string | null;
  pullRequest: number | null;
  headSha: string | null;
  reviewBinding: ReviewBinding | null;
  attempts: RunAttempts;
  usage: RunUsage;
  attentionCode: AttentionCode | null;
  attentionReason: string | null;
  resumePhase: RunPhase | null;
  createdAt: string;
  updatedAt: string;
}

export function issueLabelProjection(state: RunState, phase: RunPhase): string | null {
  switch (state) {
    case 'ready':
      return 'ready';
    case 'work_in_progress':
      return 'working';
    case 'in_review':
      return 'review';
    case 'needs_human':
      return 'human';
    case 'merged':
    case 'completed_no_change':
    case 'cancelled':
      return null;
    default:
      return assertNever(state, phase);
  }
}

function assertNever(value: never, context: unknown): never {
  throw new Error(`Unhandled value ${String(value)} in ${String(context)}`);
}
