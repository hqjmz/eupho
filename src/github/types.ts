import type { IssueSnapshot, RepositorySnapshot } from '../domain/candidate.js';

export interface RequiredCheckSnapshot {
  context: string;
  appId: number | null;
  source: 'classic_protection' | 'ruleset';
}

export interface BranchPolicySnapshot {
  strictRequiredChecks: boolean;
  dismissStaleApprovals: boolean;
  requiredApprovingReviewCount: number;
  bypassAppIds: number[];
  bypassVerificationComplete: boolean;
  requiredChecks: RequiredCheckSnapshot[];
  sources: Array<'classic_protection' | 'ruleset'>;
}

export interface GitHubReader {
  repository(repository: string, baseBranch?: string): Promise<RepositorySnapshot>;
  readyIssues(repository: string, readyLabel: string, limit?: number): Promise<IssueSnapshot[]>;
  activeIssueNumbers(repository: string, activeLabels: string[], limit?: number): Promise<number[]>;
  labelExists(repository: string, label: string): Promise<boolean>;
  branchPolicy(repository: string, branch: string): Promise<BranchPolicySnapshot>;
}
