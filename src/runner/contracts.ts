import type { ExecutionMode } from '../domain/run.js';

export interface RunnerRequest {
  runId: string;
  phase: 'author' | 'review' | 'repair';
  executionMode: ExecutionMode;
  workspacePath: string;
  prompt: string;
  deadline: string;
  turnBudget: number;
  tokenBudget: number;
}

export interface RunnerResult {
  runId: string;
  phase: RunnerRequest['phase'];
  exitCode: number;
  startedAt: string;
  finishedAt: string;
  outputArtifact: string;
  usage: {
    inputTokens: number;
    outputTokens: number;
    estimatedCostUsd: string;
  };
}

/**
 * Runner adapters receive a phase-scoped request and return an artifact. They do
 * not receive GitHub publishing credentials or mutate Eupho's lifecycle state.
 */
export interface RunnerAdapter {
  readonly name: string;
  execute(request: RunnerRequest, signal: AbortSignal): Promise<RunnerResult>;
}
