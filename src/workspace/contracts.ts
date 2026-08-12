import type { ExecutionMode, WorkspaceType } from '../domain/run.js';

export interface WorkspaceRequest {
  runId: string;
  repository: string;
  baseSha: string;
  executionMode: ExecutionMode;
  workspaceType: WorkspaceType;
}

export interface WorkspaceLease {
  runId: string;
  path: string;
  type: WorkspaceType;
  baseSha: string;
}

/**
 * This is a future dispatcher-owned port. Phase 1 deliberately provides no
 * implementation, so observe-only commands cannot create worktrees or clones.
 */
export interface WorkspaceManager {
  create(request: WorkspaceRequest): Promise<WorkspaceLease>;
  dispose(lease: WorkspaceLease): Promise<void>;
}
