import { loadHostConfig } from '../config/load.js';
import type { CandidateSnapshot } from '../domain/candidate.js';
import { CandidateStore } from '../infra/candidate-store.js';
import { defaultStateRoot, findGitWorktreeRoot, resolveSafeStateRoot } from '../infra/state-root.js';

export async function status(options: {
  cwd: string;
  stateRoot?: string;
  hostConfigPath?: string;
  environment?: NodeJS.ProcessEnv;
}): Promise<{ stateRoot: string; repositories: CandidateSnapshot[] }> {
  const stateRoot = await resolveSafeStateRoot(
    options.stateRoot ??
    (options.hostConfigPath === undefined
      ? defaultStateRoot(options.environment)
      : (await loadHostConfig(options.hostConfigPath)).value.stateRoot),
    await findGitWorktreeRoot(options.cwd),
  );
  return { stateRoot, repositories: await new CandidateStore(stateRoot).list() };
}
