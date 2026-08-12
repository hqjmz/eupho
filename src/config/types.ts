import type { ExecutionMode, MergePolicy, WorkspaceType } from '../domain/run.js';

export interface RepositoryConfig {
  version: 1;
  baseBranch: string;
  concurrency: number;
  pollIntervalSeconds: number;
  mergePolicy: MergePolicy;
  execution: {
    defaultMode: ExecutionMode;
    attended: {
      workspace: 'worktree';
      nativePermissionPrompts: true;
      sandboxProfile: string;
    };
    unattended: {
      workspace: 'ephemeral_clone';
      nativePermissionPrompts: false;
      sandboxProfile: string;
    };
  };
  githubApp: {
    slug: string;
    requiredCheckSource: string;
  };
  labels: {
    ready: string;
    working: string;
    review: string;
    human: string;
  };
  routing: {
    autonomousClasses: Array<{
      label: string;
      executionMode: 'unattended';
      allowedPaths: string[];
    }>;
  };
  branches: {
    pattern: string;
    mergeMethod: 'merge' | 'squash' | 'rebase';
    requireUpToDate: boolean;
    dismissStaleApprovals: boolean;
    mergeQueue: boolean;
  };
  runners: {
    author: RunnerProfile;
    reviewer: RunnerProfile & { requireIndependentContext: boolean };
  };
  limits: {
    authorMinutes: number;
    reviewMinutes: number;
    repairCycles: number;
    modelTurnsPerPhase: number;
    modelTokensPerRun: number;
    modelCostUsdPerRun: string;
    modelCostUsdPerRepoDay: string;
    priceTableProfile: string;
    maxChangedFiles: number;
    maxDiffLines: number;
  };
  review: {
    requiredCheck: string;
    blockingSeverities: string[];
    alwaysBlockingCategories: string[];
    baseDriftPolicy: 'full_rereview';
    advisoryHostedReviews: boolean;
    enableAutoMerge: boolean;
  };
  validation: {
    commands: Array<{ name: string; argv: string[] }>;
  };
  policy: {
    protectedPaths: string[];
  };
  notifications: {
    events: string[];
    sinks: string[];
  };
}

export interface RunnerProfile {
  adapter: string;
  profile: string;
}

export interface HostConfig {
  version: 1;
  stateRoot: string;
  workspaceRoot: string;
  metadataSigning: {
    currentKeyId: string;
    keyFile: string;
    verificationKeyFiles: Record<string, string>;
  };
  githubApp: {
    appId: number;
    privateKeyFile: string;
  };
  sandboxProfiles: Record<
    string,
    {
      backend: 'container' | 'vm';
      network: 'deny_by_default';
      runnerStateAccess: 'none';
      sharedGitAdmin: false;
    }
  >;
  workspaceProfiles: Record<
    string,
    {
      sharedObjects: false;
      authenticatedRemote: false;
    }
  >;
  priceTableProfiles: Record<string, string>;
  notificationSinks: Record<
    string,
    {
      argv: string[];
      timeoutSeconds: number;
    }
  >;
}

export interface LoadedConfig<T> {
  source: string;
  value: T;
}

export const REPOSITORY_CONFIG_PATHS = ['.github/eupho.yml', '.github/agent-orchestrator.yml'] as const;

export function workspaceForMode(config: RepositoryConfig, mode: ExecutionMode): WorkspaceType {
  return mode === 'attended'
    ? config.execution.attended.workspace
    : config.execution.unattended.workspace;
}
