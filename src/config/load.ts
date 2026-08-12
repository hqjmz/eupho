import { access, readFile } from 'node:fs/promises';
import { isAbsolute, normalize, parse as parsePath, resolve } from 'node:path';

import { parse } from 'yaml';

import { EuphoError, messageOf } from '../errors.js';
import { pathsOverlap } from '../infra/state-root.js';
import type { HostConfig, LoadedConfig, RepositoryConfig, RunnerProfile } from './types.js';
import { REPOSITORY_CONFIG_PATHS } from './types.js';

type ObjectValue = Record<string, unknown>;

export async function findRepositoryConfig(cwd: string, explicit?: string): Promise<string> {
  if (explicit !== undefined) {
    return resolve(cwd, explicit);
  }

  for (const candidate of REPOSITORY_CONFIG_PATHS) {
    const absolute = resolve(cwd, candidate);
    try {
      await access(absolute);
      return absolute;
    } catch {
      // Try the next supported path.
    }
  }

  throw new EuphoError(
    'config_not_found',
    `No repository policy found. Tried ${REPOSITORY_CONFIG_PATHS.join(', ')}`,
  );
}

export async function loadRepositoryConfig(path: string): Promise<LoadedConfig<RepositoryConfig>> {
  return { source: path, value: parseRepositoryConfigText(await readText(path), path) };
}

export async function loadHostConfig(path: string): Promise<LoadedConfig<HostConfig>> {
  return { source: path, value: parseHostConfigText(await readText(path), path) };
}

export function parseRepositoryConfigText(text: string, source = '<repository-policy>'): RepositoryConfig {
  const root = parseYaml(text, source);
  assertKeys(
    root,
    [
      'version',
      'base_branch',
      'concurrency',
      'poll_interval_seconds',
      'merge_policy',
      'execution',
      'github_app',
      'labels',
      'routing',
      'branches',
      'runners',
      'limits',
      'review',
      'validation',
      'policy',
      'notifications',
    ],
    source,
  );

  const version = integer(root.version, `${source}.version`, 1, 1);
  if (version !== 1) throw validationError(`${source}.version`, 'must be 1');

  const execution = object(root.execution, `${source}.execution`);
  assertKeys(execution, ['default_mode', 'attended', 'unattended'], `${source}.execution`);
  const defaultMode = enumeration(execution.default_mode, `${source}.execution.default_mode`, [
    'attended',
    'unattended',
  ] as const);
  const attended = object(execution.attended, `${source}.execution.attended`);
  assertKeys(
    attended,
    ['workspace', 'native_permission_prompts', 'sandbox_profile'],
    `${source}.execution.attended`,
  );
  const attendedWorkspace = enumeration(
    attended.workspace,
    `${source}.execution.attended.workspace`,
    ['worktree'] as const,
  );
  const attendedPrompts = boolean(
    attended.native_permission_prompts,
    `${source}.execution.attended.native_permission_prompts`,
  );
  if (!attendedPrompts) {
    throw validationError(`${source}.execution.attended.native_permission_prompts`, 'must be true');
  }

  const unattended = object(execution.unattended, `${source}.execution.unattended`);
  assertKeys(
    unattended,
    ['workspace', 'native_permission_prompts', 'sandbox_profile'],
    `${source}.execution.unattended`,
  );
  const unattendedWorkspace = enumeration(
    unattended.workspace,
    `${source}.execution.unattended.workspace`,
    ['ephemeral_clone'] as const,
  );
  const unattendedPrompts = boolean(
    unattended.native_permission_prompts,
    `${source}.execution.unattended.native_permission_prompts`,
  );
  if (unattendedPrompts) {
    throw validationError(`${source}.execution.unattended.native_permission_prompts`, 'must be false');
  }

  const githubApp = object(root.github_app, `${source}.github_app`);
  assertKeys(githubApp, ['slug', 'required_check_source'], `${source}.github_app`);

  const labels = object(root.labels, `${source}.labels`);
  assertKeys(labels, ['ready', 'working', 'review', 'human'], `${source}.labels`);
  const compiledLabels = {
    ready: nonEmptyString(labels.ready, `${source}.labels.ready`),
    working: nonEmptyString(labels.working, `${source}.labels.working`),
    review: nonEmptyString(labels.review, `${source}.labels.review`),
    human: nonEmptyString(labels.human, `${source}.labels.human`),
  };
  if (new Set(Object.values(compiledLabels)).size !== 4) {
    throw validationError(`${source}.labels`, 'all workflow-state labels must be distinct');
  }

  const routing = object(root.routing, `${source}.routing`);
  assertKeys(routing, ['autonomous_classes'], `${source}.routing`);
  const autonomousClasses = array(routing.autonomous_classes, `${source}.routing.autonomous_classes`).map(
    (entry, index) => {
      const path = `${source}.routing.autonomous_classes[${index}]`;
      const value = object(entry, path);
      assertKeys(value, ['label', 'execution_mode', 'allowed_paths'], path);
      return {
        label: nonEmptyString(value.label, `${path}.label`),
        executionMode: enumeration(value.execution_mode, `${path}.execution_mode`, [
          'unattended',
        ] as const),
        allowedPaths: nonEmptyStringArray(value.allowed_paths, `${path}.allowed_paths`),
      };
    },
  );
  const autonomousLabels = autonomousClasses.map((entry) => entry.label);
  if (new Set(autonomousLabels).size !== autonomousLabels.length) {
    throw validationError(
      `${source}.routing.autonomous_classes`,
      'must use a distinct label for every autonomous class',
    );
  }
  for (const label of autonomousLabels) {
    if (Object.values(compiledLabels).includes(label)) {
      throw validationError(
        `${source}.routing.autonomous_classes`,
        `label ${label} must not be a workflow-state label`,
      );
    }
  }

  const branches = object(root.branches, `${source}.branches`);
  assertKeys(
    branches,
    ['pattern', 'merge_method', 'require_up_to_date', 'dismiss_stale_approvals', 'merge_queue'],
    `${source}.branches`,
  );

  const runners = object(root.runners, `${source}.runners`);
  assertKeys(runners, ['author', 'reviewer'], `${source}.runners`);
  const author = parseRunner(runners.author, `${source}.runners.author`, false);
  const reviewerBase = parseRunner(runners.reviewer, `${source}.runners.reviewer`, true);
  const reviewerObject = object(runners.reviewer, `${source}.runners.reviewer`);

  const limits = object(root.limits, `${source}.limits`);
  assertKeys(
    limits,
    [
      'author_minutes',
      'review_minutes',
      'repair_cycles',
      'model_turns_per_phase',
      'model_tokens_per_run',
      'model_cost_usd_per_run',
      'model_cost_usd_per_repo_day',
      'price_table_profile',
      'max_changed_files',
      'max_diff_lines',
    ],
    `${source}.limits`,
  );

  const review = object(root.review, `${source}.review`);
  assertKeys(
    review,
    [
      'required_check',
      'blocking_severities',
      'always_blocking_categories',
      'base_drift_policy',
      'advisory_hosted_reviews',
      'enable_auto_merge',
    ],
    `${source}.review`,
  );

  const validation = object(root.validation, `${source}.validation`);
  assertKeys(validation, ['commands'], `${source}.validation`);
  const commands = array(validation.commands, `${source}.validation.commands`).map((entry, index) => {
    const path = `${source}.validation.commands[${index}]`;
    const value = object(entry, path);
    assertKeys(value, ['name', 'argv'], path);
    return {
      name: nonEmptyString(value.name, `${path}.name`),
      argv: nonEmptyStringArray(value.argv, `${path}.argv`),
    };
  });

  const policy = object(root.policy, `${source}.policy`);
  assertKeys(policy, ['protected_paths'], `${source}.policy`);

  const notifications = object(root.notifications, `${source}.notifications`);
  assertKeys(notifications, ['events', 'sinks'], `${source}.notifications`);

  const mergePolicy = enumeration(root.merge_policy, `${source}.merge_policy`, [
    'autonomous-low-risk',
    'human-final-approval',
    'suggest-only',
  ] as const);
  if (mergePolicy === 'autonomous-low-risk') {
    throw validationError(
      `${source}.merge_policy`,
      'cannot be autonomous-low-risk globally; use an explicit routing.autonomous_classes entry',
    );
  }
  const mergeQueue = boolean(branches.merge_queue, `${source}.branches.merge_queue`);
  if (mergeQueue) throw validationError(`${source}.branches.merge_queue`, 'must be false in version 1');
  const requireUpToDate = boolean(
    branches.require_up_to_date,
    `${source}.branches.require_up_to_date`,
  );
  if (!requireUpToDate) {
    throw validationError(`${source}.branches.require_up_to_date`, 'must be true in version 1');
  }
  const dismissStaleApprovals = boolean(
    branches.dismiss_stale_approvals,
    `${source}.branches.dismiss_stale_approvals`,
  );
  if (mergePolicy === 'human-final-approval' && !dismissStaleApprovals) {
    throw validationError(
      `${source}.branches.dismiss_stale_approvals`,
      'must be true for human-final-approval',
    );
  }
  const appSlug = nonEmptyString(githubApp.slug, `${source}.github_app.slug`);
  const requiredCheckSource = nonEmptyString(
    githubApp.required_check_source,
    `${source}.github_app.required_check_source`,
  );
  if (requiredCheckSource !== appSlug) {
    throw validationError(
      `${source}.github_app.required_check_source`,
      'must match github_app.slug in version 1',
    );
  }
  const alwaysBlockingCategories = nonEmptyStringArray(
    review.always_blocking_categories,
    `${source}.review.always_blocking_categories`,
  );
  if (!alwaysBlockingCategories.includes('weakened_or_deleted_tests')) {
    throw validationError(
      `${source}.review.always_blocking_categories`,
      'must include weakened_or_deleted_tests',
    );
  }
  const enableAutoMerge = boolean(review.enable_auto_merge, `${source}.review.enable_auto_merge`);
  if (mergePolicy === 'suggest-only' && enableAutoMerge) {
    throw validationError(`${source}.review.enable_auto_merge`, 'must be false for suggest-only');
  }
  if (mergePolicy === 'human-final-approval' && !enableAutoMerge) {
    throw validationError(
      `${source}.review.enable_auto_merge`,
      'must be true for the native human-final-approval wait',
    );
  }

  return {
    version: 1,
    baseBranch: nonEmptyString(root.base_branch, `${source}.base_branch`),
    concurrency: integer(root.concurrency, `${source}.concurrency`, 1, 32),
    pollIntervalSeconds: integer(
      root.poll_interval_seconds,
      `${source}.poll_interval_seconds`,
      1,
      3600,
    ),
    mergePolicy,
    execution: {
      defaultMode,
      attended: {
        workspace: attendedWorkspace,
        nativePermissionPrompts: true,
        sandboxProfile: nonEmptyString(
          attended.sandbox_profile,
          `${source}.execution.attended.sandbox_profile`,
        ),
      },
      unattended: {
        workspace: unattendedWorkspace,
        nativePermissionPrompts: false,
        sandboxProfile: nonEmptyString(
          unattended.sandbox_profile,
          `${source}.execution.unattended.sandbox_profile`,
        ),
      },
    },
    githubApp: {
      slug: appSlug,
      requiredCheckSource,
    },
    labels: compiledLabels,
    routing: { autonomousClasses },
    branches: {
      pattern: nonEmptyString(branches.pattern, `${source}.branches.pattern`),
      mergeMethod: enumeration(branches.merge_method, `${source}.branches.merge_method`, [
        'merge',
        'squash',
        'rebase',
      ] as const),
      requireUpToDate,
      dismissStaleApprovals,
      mergeQueue,
    },
    runners: {
      author,
      reviewer: {
        ...reviewerBase,
        requireIndependentContext: boolean(
          reviewerObject.require_independent_context,
          `${source}.runners.reviewer.require_independent_context`,
        ),
      },
    },
    limits: {
      authorMinutes: integer(limits.author_minutes, `${source}.limits.author_minutes`, 1),
      reviewMinutes: integer(limits.review_minutes, `${source}.limits.review_minutes`, 1),
      repairCycles: integer(limits.repair_cycles, `${source}.limits.repair_cycles`, 0),
      modelTurnsPerPhase: integer(
        limits.model_turns_per_phase,
        `${source}.limits.model_turns_per_phase`,
        1,
      ),
      modelTokensPerRun: integer(
        limits.model_tokens_per_run,
        `${source}.limits.model_tokens_per_run`,
        1,
      ),
      modelCostUsdPerRun: decimalString(
        limits.model_cost_usd_per_run,
        `${source}.limits.model_cost_usd_per_run`,
      ),
      modelCostUsdPerRepoDay: decimalString(
        limits.model_cost_usd_per_repo_day,
        `${source}.limits.model_cost_usd_per_repo_day`,
      ),
      priceTableProfile: nonEmptyString(
        limits.price_table_profile,
        `${source}.limits.price_table_profile`,
      ),
      maxChangedFiles: integer(limits.max_changed_files, `${source}.limits.max_changed_files`, 1),
      maxDiffLines: integer(limits.max_diff_lines, `${source}.limits.max_diff_lines`, 1),
    },
    review: {
      requiredCheck: nonEmptyString(review.required_check, `${source}.review.required_check`),
      blockingSeverities: nonEmptyStringArray(
        review.blocking_severities,
        `${source}.review.blocking_severities`,
      ),
      alwaysBlockingCategories,
      baseDriftPolicy: enumeration(
        review.base_drift_policy,
        `${source}.review.base_drift_policy`,
        ['full_rereview'] as const,
      ),
      advisoryHostedReviews: boolean(
        review.advisory_hosted_reviews,
        `${source}.review.advisory_hosted_reviews`,
      ),
      enableAutoMerge,
    },
    validation: { commands },
    policy: {
      protectedPaths: nonEmptyStringArray(policy.protected_paths, `${source}.policy.protected_paths`),
    },
    notifications: {
      events: nonEmptyStringArray(notifications.events, `${source}.notifications.events`),
      sinks: nonEmptyStringArray(notifications.sinks, `${source}.notifications.sinks`),
    },
  };
}

export function parseHostConfigText(text: string, source = '<host-config>'): HostConfig {
  const root = parseYaml(text, source);
  assertKeys(
    root,
    [
      'version',
      'state_root',
      'workspace_root',
      'metadata_signing',
      'github_app',
      'sandbox_profiles',
      'workspace_profiles',
      'price_table_profiles',
      'notification_sinks',
    ],
    source,
  );
  if (integer(root.version, `${source}.version`, 1, 1) !== 1) {
    throw validationError(`${source}.version`, 'must be 1');
  }

  const signing = object(root.metadata_signing, `${source}.metadata_signing`);
  assertKeys(
    signing,
    ['current_key_id', 'key_file', 'verification_key_files'],
    `${source}.metadata_signing`,
  );
  const app = object(root.github_app, `${source}.github_app`);
  assertKeys(app, ['app_id', 'private_key_file'], `${source}.github_app`);

  const sandboxProfiles = mapObject(
    root.sandbox_profiles,
    `${source}.sandbox_profiles`,
    (value, path) => {
      assertKeys(value, ['backend', 'network', 'runner_state_access', 'shared_git_admin'], path);
      const sharedGitAdmin = boolean(value.shared_git_admin, `${path}.shared_git_admin`);
      if (sharedGitAdmin) throw validationError(`${path}.shared_git_admin`, 'must be false');
      return {
        backend: enumeration(value.backend, `${path}.backend`, ['container', 'vm'] as const),
        network: enumeration(value.network, `${path}.network`, ['deny_by_default'] as const),
        runnerStateAccess: enumeration(
          value.runner_state_access,
          `${path}.runner_state_access`,
          ['none'] as const,
        ),
        sharedGitAdmin: false as const,
      };
    },
  );

  const workspaceProfiles = mapObject(
    root.workspace_profiles,
    `${source}.workspace_profiles`,
    (value, path) => {
      assertKeys(value, ['shared_objects', 'authenticated_remote'], path);
      const sharedObjects = boolean(value.shared_objects, `${path}.shared_objects`);
      const authenticatedRemote = boolean(
        value.authenticated_remote,
        `${path}.authenticated_remote`,
      );
      if (sharedObjects) throw validationError(`${path}.shared_objects`, 'must be false');
      if (authenticatedRemote) {
        throw validationError(`${path}.authenticated_remote`, 'must be false');
      }
      return { sharedObjects: false as const, authenticatedRemote: false as const };
    },
  );

  const priceTableProfiles = mapStringObject(
    root.price_table_profiles,
    `${source}.price_table_profiles`,
  );
  const notificationSinks = mapObject(
    root.notification_sinks,
    `${source}.notification_sinks`,
    (value, path) => {
      assertKeys(value, ['argv', 'timeout_seconds'], path);
      const argv = nonEmptyStringArray(value.argv, `${path}.argv`);
      if (!isAbsolute(argv[0] ?? '')) {
        throw validationError(`${path}.argv[0]`, 'must be an absolute executable path');
      }
      return {
        argv,
        timeoutSeconds: integer(value.timeout_seconds, `${path}.timeout_seconds`, 1, 3600),
      };
    },
  );

  const stateRoot = absolutePath(root.state_root, `${source}.state_root`);
  const workspaceRoot = absolutePath(root.workspace_root, `${source}.workspace_root`);
  if (pathsOverlap(stateRoot, workspaceRoot)) {
    throw validationError(`${source}.state_root`, 'must not overlap workspace_root');
  }

  return {
    version: 1,
    stateRoot,
    workspaceRoot,
    metadataSigning: {
      currentKeyId: nonEmptyString(signing.current_key_id, `${source}.metadata_signing.current_key_id`),
      keyFile: absolutePath(signing.key_file, `${source}.metadata_signing.key_file`),
      verificationKeyFiles: mapStringObject(
        signing.verification_key_files,
        `${source}.metadata_signing.verification_key_files`,
      ),
    },
    githubApp: {
      appId: integer(app.app_id, `${source}.github_app.app_id`, 1),
      privateKeyFile: absolutePath(app.private_key_file, `${source}.github_app.private_key_file`),
    },
    sandboxProfiles,
    workspaceProfiles,
    priceTableProfiles,
    notificationSinks,
  };
}

function parseRunner(value: unknown, path: string, reviewer: boolean): RunnerProfile {
  const runner = object(value, path);
  assertKeys(runner, reviewer ? ['adapter', 'profile', 'require_independent_context'] : ['adapter', 'profile'], path);
  return {
    adapter: nonEmptyString(runner.adapter, `${path}.adapter`),
    profile: nonEmptyString(runner.profile, `${path}.profile`),
  };
}

function parseYaml(text: string, source: string): ObjectValue {
  try {
    return object(parse(text), source);
  } catch (error) {
    if (error instanceof EuphoError) throw error;
    throw new EuphoError('invalid_config', `Cannot parse ${source}: ${messageOf(error)}`, 1, {
      cause: error,
    });
  }
}

async function readText(path: string): Promise<string> {
  try {
    return await readFile(path, 'utf8');
  } catch (error) {
    throw new EuphoError('config_read_failed', `Cannot read ${path}: ${messageOf(error)}`, 1, {
      cause: error,
    });
  }
}

function object(value: unknown, path: string): ObjectValue {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw validationError(path, 'must be an object');
  }
  return value as ObjectValue;
}

function array(value: unknown, path: string): unknown[] {
  if (!Array.isArray(value)) throw validationError(path, 'must be an array');
  return value;
}

function nonEmptyString(value: unknown, path: string): string {
  if (typeof value !== 'string' || value.trim().length === 0) {
    throw validationError(path, 'must be a non-empty string');
  }
  return value;
}

function nonEmptyStringArray(value: unknown, path: string): string[] {
  const values = array(value, path).map((entry, index) => nonEmptyString(entry, `${path}[${index}]`));
  if (values.length === 0) throw validationError(path, 'must not be empty');
  return values;
}

function integer(value: unknown, path: string, minimum: number, maximum = Number.MAX_SAFE_INTEGER): number {
  if (!Number.isSafeInteger(value) || (value as number) < minimum || (value as number) > maximum) {
    throw validationError(path, `must be an integer between ${minimum} and ${maximum}`);
  }
  return value as number;
}

function boolean(value: unknown, path: string): boolean {
  if (typeof value !== 'boolean') throw validationError(path, 'must be a boolean');
  return value;
}

function decimalString(value: unknown, path: string): string {
  const result = nonEmptyString(value, path);
  if (!/^(?:0|[1-9]\d*)(?:\.\d{1,6})?$/.test(result)) {
    throw validationError(path, 'must be a non-negative decimal string with at most six decimals');
  }
  return result;
}

function absolutePath(value: unknown, path: string): string {
  const result = nonEmptyString(value, path);
  if (!isAbsolute(result)) throw validationError(path, 'must be an absolute path');
  const normalized = normalize(result);
  if (normalized === parsePath(normalized).root) {
    throw validationError(path, 'must not resolve to the filesystem root');
  }
  return normalized;
}

function enumeration<const T extends readonly string[]>(value: unknown, path: string, allowed: T): T[number] {
  if (typeof value !== 'string' || !allowed.includes(value)) {
    throw validationError(path, `must be one of ${allowed.join(', ')}`);
  }
  return value as T[number];
}

function assertKeys(value: ObjectValue, allowed: string[], path: string): void {
  const allowedSet = new Set(allowed);
  const unknown = Object.keys(value).filter((key) => !allowedSet.has(key));
  if (unknown.length > 0) {
    throw validationError(path, `contains unknown field${unknown.length === 1 ? '' : 's'}: ${unknown.join(', ')}`);
  }
  const missing = allowed.filter((key) => !(key in value));
  if (missing.length > 0) {
    throw validationError(path, `is missing required field${missing.length === 1 ? '' : 's'}: ${missing.join(', ')}`);
  }
}

function mapObject<T>(
  value: unknown,
  path: string,
  compile: (entry: ObjectValue, entryPath: string) => T,
): Record<string, T> {
  const source = object(value, path);
  const result: Record<string, T> = Object.create(null) as Record<string, T>;
  for (const [key, entry] of Object.entries(source)) {
    assertMapKey(key, path);
    result[key] = compile(object(entry, `${path}.${key}`), `${path}.${key}`);
  }
  return result;
}

function mapStringObject(value: unknown, path: string): Record<string, string> {
  const source = object(value, path);
  const result: Record<string, string> = Object.create(null) as Record<string, string>;
  for (const [key, entry] of Object.entries(source)) {
    assertMapKey(key, path);
    result[key] = absolutePath(entry, `${path}.${key}`);
  }
  return result;
}

function assertMapKey(key: string, path: string): void {
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(key)) {
    throw validationError(`${path}.${key}`, 'has an unsafe profile name');
  }
}

function validationError(path: string, detail: string): EuphoError {
  return new EuphoError('invalid_config', `${path} ${detail}`);
}
