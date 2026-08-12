import type { RepositoryConfig } from '../config/types.js';
import { findRepositoryConfig, loadRepositoryConfig, parseRepositoryConfigText } from '../config/load.js';
import type { RepositorySnapshot } from '../domain/candidate.js';
import { EuphoError } from '../errors.js';
import type { GitHubReader } from '../github/types.js';

export interface ResolvedPolicy {
  repository: RepositorySnapshot;
  config: RepositoryConfig;
  source: string;
  trustedBase: boolean;
}

export async function resolvePolicy(options: {
  reader: GitHubReader;
  repository: string;
  cwd: string;
  explicitConfig?: string;
}): Promise<ResolvedPolicy> {
  if (options.explicitConfig !== undefined) {
    const path = await findRepositoryConfig(options.cwd, options.explicitConfig);
    const loaded = await loadRepositoryConfig(path);
    const repository = await options.reader.repository(options.repository, loaded.value.baseBranch);
    return {
      repository,
      config: loaded.value,
      source: loaded.source,
      trustedBase: false,
    };
  }

  let repository = await options.reader.repository(options.repository);
  let config = parseRemotePolicy(repository);
  if (config.baseBranch !== repository.defaultBranch) {
    const selectedBaseBranch = config.baseBranch;
    repository = await options.reader.repository(options.repository, selectedBaseBranch);
    config = parseRemotePolicy(repository);
    if (config.baseBranch !== selectedBaseBranch) {
      throw new EuphoError(
        'unstable_policy_base',
        `Policy selected ${selectedBaseBranch}, but that branch selected ${config.baseBranch}`,
      );
    }
  }
  return {
    repository,
    config,
    source: `github:${repository.nameWithOwner}/${repository.policyPath ?? '<missing>'}@${repository.baseSha}`,
    trustedBase: true,
  };
}

function parseRemotePolicy(repository: RepositorySnapshot): RepositoryConfig {
  if (repository.policyPath === null || repository.policyContent === null) {
    throw new EuphoError(
      'remote_policy_not_found',
      `No Eupho repository policy exists on ${repository.nameWithOwner}@${repository.baseSha}`,
    );
  }
  return parseRepositoryConfigText(
    repository.policyContent,
    `github:${repository.nameWithOwner}/${repository.policyPath}@${repository.baseSha}`,
  );
}
