import { execFile } from 'node:child_process';
import { promisify } from 'node:util';

import { REPOSITORY_CONFIG_PATHS } from '../config/types.js';
import type { IssueSnapshot, RepositorySnapshot } from '../domain/candidate.js';
import { EuphoError, messageOf } from '../errors.js';
import type {
  BranchPolicySnapshot,
  GitHubReader,
  RequiredCheckSnapshot,
} from './types.js';

const execFileAsync = promisify(execFile);

interface GhReaderOptions {
  binary?: string;
  token?: string;
  environment?: NodeJS.ProcessEnv;
}

export class GhReader implements GitHubReader {
  private readonly binary: string;
  private readonly environment: NodeJS.ProcessEnv;

  constructor(options: GhReaderOptions = {}) {
    this.binary = options.binary ?? 'gh';
    this.environment = { ...(options.environment ?? process.env) };
    this.environment.GH_PROMPT_DISABLED = '1';
    this.environment.GH_NO_UPDATE_NOTIFIER = '1';
    if (options.token !== undefined) this.environment.GH_TOKEN = options.token;
  }

  async repository(repository: string, configuredBaseBranch?: string): Promise<RepositorySnapshot> {
    assertRepository(repository);
    const metadata = parseRepositoryMetadata(await this.api<unknown>(`repos/${repository}`));
    const baseBranch = configuredBaseBranch ?? metadata.default_branch;
    const commit = parseCommit(
      await this.api<unknown>(`repos/${repository}/commits/${encodeURIComponent(baseBranch)}`),
    );

    let policyPath: string | null = null;
    let policyContent: string | null = null;
    for (const candidate of REPOSITORY_CONFIG_PATHS) {
      const rawContent = await this.apiOptional<unknown>(
        `repos/${repository}/contents/${candidate}`,
        ['--field', `ref=${commit.sha}`],
      );
      if (rawContent === null) continue;
      const content = parseContent(rawContent, candidate);
      if (content.encoding !== 'base64' || typeof content.content !== 'string') {
        throw new EuphoError('invalid_remote_policy', `${candidate} is not a base64 GitHub content object`);
      }
      policyPath = candidate;
      policyContent = Buffer.from(content.content.replace(/\s/g, ''), 'base64').toString('utf8');
      break;
    }

    return {
      id: metadata.id,
      nameWithOwner: metadata.full_name,
      defaultBranch: metadata.default_branch,
      baseSha: commit.sha,
      policyPath,
      policyContent,
    };
  }

  async readyIssues(repository: string, readyLabel: string, limit = 100): Promise<IssueSnapshot[]> {
    assertRepository(repository);
    const result = await this.run([
      'issue',
      'list',
      '--repo',
      repository,
      '--state',
      'open',
      '--label',
      readyLabel,
      '--limit',
      String(limit),
      '--json',
      'number,title,url,labels,updatedAt',
    ]);
    const issues = parseIssueList(parseJson<unknown>(result, 'gh issue list'));
    return issues.map((issue) => ({
      number: issue.number,
      title: issue.title,
      url: issue.url,
      labels: issue.labels.map((label) => label.name),
      updatedAt: issue.updatedAt,
    }));
  }

  async activeIssueNumbers(repository: string, activeLabels: string[], limit = 100): Promise<number[]> {
    assertRepository(repository);
    const active = new Set<number>();
    for (const label of activeLabels) {
      const result = await this.run([
        'issue',
        'list',
        '--repo',
        repository,
        '--state',
        'open',
        '--label',
        label,
        '--limit',
        String(limit),
        '--json',
        'number',
      ]);
      for (const issue of parseIssueNumbers(parseJson<unknown>(result, 'gh issue list'))) {
        active.add(issue.number);
      }
    }
    return [...active].sort((left, right) => left - right);
  }

  async labelExists(repository: string, label: string): Promise<boolean> {
    assertRepository(repository);
    return (
      (await this.apiOptional(
        `repos/${repository}/labels/${encodeURIComponent(label)}`,
      )) !== null
    );
  }

  async branchPolicy(repository: string, branch: string): Promise<BranchPolicySnapshot> {
    assertRepository(repository);
    const classicRaw = await this.apiOptional<unknown>(
      `repos/${repository}/branches/${encodeURIComponent(branch)}/protection`,
    );
    const rulesRaw = await this.apiOptional<unknown>(
      `repos/${repository}/rules/branches/${encodeURIComponent(branch)}`,
      ['--field', 'per_page=100'],
    );
    const classic = classicRaw === null ? null : parseClassicProtection(classicRaw);
    const rules = rulesRaw === null ? null : parseActiveRules(rulesRaw);

    const requiredChecks: RequiredCheckSnapshot[] = [];
    const sources: BranchPolicySnapshot['sources'] = [];
    let strictRequiredChecks = false;
    let dismissStaleApprovals = false;
    let requiredApprovingReviewCount = 0;
    const bypassAppIds = new Set<number>();
    let bypassVerificationComplete = true;

    if (classic !== null) {
      sources.push('classic_protection');
      strictRequiredChecks ||= classic.required_status_checks?.strict === true;
      dismissStaleApprovals ||=
        classic.required_pull_request_reviews?.dismiss_stale_reviews === true;
      requiredApprovingReviewCount = Math.max(
        requiredApprovingReviewCount,
        classic.required_pull_request_reviews?.required_approving_review_count ?? 0,
      );
      for (const app of classic.required_pull_request_reviews?.bypass_pull_request_allowances
        ?.apps ?? []) {
        bypassAppIds.add(app.id);
      }
      for (const check of classic.required_status_checks?.checks ?? []) {
        requiredChecks.push({
          context: check.context,
          appId: typeof check.app_id === 'number' ? check.app_id : null,
          source: 'classic_protection',
        });
      }
      for (const context of classic.required_status_checks?.contexts ?? []) {
        if (!requiredChecks.some((check) => check.context === context)) {
          requiredChecks.push({ context, appId: null, source: 'classic_protection' });
        }
      }
    }

    if (rules !== null) {
      let relevantRuleSeen = false;
      const relevantRulesetIds = new Set<number>();
      for (const rule of rules) {
        if (rule.type === 'required_status_checks') {
          relevantRuleSeen = true;
          if (rule.ruleset_id === undefined) {
            throw new EuphoError(
              'invalid_github_response',
              'A required_status_checks rule is missing ruleset_id',
            );
          }
          relevantRulesetIds.add(rule.ruleset_id);
          strictRequiredChecks ||= rule.parameters?.strict_required_status_checks_policy === true;
          for (const check of rule.parameters?.required_status_checks ?? []) {
            requiredChecks.push({
              context: check.context,
              appId: typeof check.integration_id === 'number' ? check.integration_id : null,
              source: 'ruleset',
            });
          }
        }
        if (rule.type === 'pull_request') {
          relevantRuleSeen = true;
          if (rule.ruleset_id === undefined) {
            throw new EuphoError(
              'invalid_github_response',
              'A pull_request rule is missing ruleset_id',
            );
          }
          relevantRulesetIds.add(rule.ruleset_id);
          dismissStaleApprovals ||= rule.parameters?.dismiss_stale_reviews_on_push === true;
          requiredApprovingReviewCount = Math.max(
            requiredApprovingReviewCount,
            rule.parameters?.required_approving_review_count ?? 0,
          );
        }
      }
      if (relevantRuleSeen) {
        sources.push('ruleset');
        for (const rulesetId of relevantRulesetIds) {
          const detail = parseRulesetDetail(
            await this.api<unknown>(`repos/${repository}/rulesets/${rulesetId}`, [
              '--field',
              'includes_parents=true',
            ]),
          );
          bypassVerificationComplete &&= detail.bypassActors !== undefined;
          for (const actor of detail.bypassActors ?? []) {
            if (actor.actorType === 'Integration' && actor.actorId !== null) {
              bypassAppIds.add(actor.actorId);
            }
          }
        }
      }
    }

    return {
      strictRequiredChecks,
      dismissStaleApprovals,
      requiredApprovingReviewCount,
      bypassAppIds: [...bypassAppIds].sort((left, right) => left - right),
      bypassVerificationComplete,
      requiredChecks,
      sources: [...new Set(sources)],
    };
  }

  private async api<T>(endpoint: string, extra: string[] = []): Promise<T> {
    return parseJson<T>(
      await this.run([
        'api',
        '--method',
        'GET',
        endpoint,
        '--header',
        'Accept: application/vnd.github+json',
        '--header',
        'X-GitHub-Api-Version: 2026-03-10',
        ...extra,
      ]),
      `gh api ${endpoint}`,
    );
  }

  private async apiOptional<T>(endpoint: string, extra: string[] = []): Promise<T | null> {
    try {
      return await this.api<T>(endpoint, extra);
    } catch (error) {
      if (isNotFound(error)) return null;
      throw error;
    }
  }

  private async run(arguments_: string[]): Promise<string> {
    try {
      const result = await execFileAsync(this.binary, arguments_, {
        encoding: 'utf8',
        env: this.environment,
        maxBuffer: 10 * 1024 * 1024,
        timeout: 30_000,
        killSignal: 'SIGTERM',
      });
      return result.stdout;
    } catch (error) {
      const stderr =
        error !== null && typeof error === 'object' && 'stderr' in error
          ? String((error as { stderr: unknown }).stderr).trim()
          : '';
      throw new EuphoError(
        'github_read_failed',
        `GitHub read failed (${this.binary} ${arguments_.join(' ')}): ${stderr || messageOf(error)}`,
        1,
        { cause: error },
      );
    }
  }
}

interface ClassicProtection {
  required_status_checks?: {
    strict?: boolean | undefined;
    contexts?: string[] | undefined;
    checks?: Array<{ context: string; app_id?: number | null | undefined }> | undefined;
  };
  required_pull_request_reviews?: {
    dismiss_stale_reviews?: boolean | undefined;
    required_approving_review_count?: number | undefined;
    bypass_pull_request_allowances?:
      | { apps?: Array<{ id: number }> | undefined }
      | undefined;
  };
}

interface ActiveRule {
  type: string;
  ruleset_id?: number | undefined;
  parameters?: {
    strict_required_status_checks_policy?: boolean | undefined;
    required_status_checks?:
      | Array<{ context: string; integration_id?: number | null | undefined }>
      | undefined;
    dismiss_stale_reviews_on_push?: boolean | undefined;
    required_approving_review_count?: number | undefined;
  };
}

interface IssueResponse {
  number: number;
  title: string;
  url: string;
  labels: Array<{ name: string }>;
  updatedAt: string;
}

function parseRepositoryMetadata(value: unknown): {
  id: number;
  full_name: string;
  default_branch: string;
} {
  const object = responseObject(value, 'repository metadata');
  return {
    id: positiveInteger(object.id, 'repository metadata.id'),
    full_name: repositoryName(object.full_name, 'repository metadata.full_name'),
    default_branch: responseString(object.default_branch, 'repository metadata.default_branch'),
  };
}

function parseCommit(value: unknown): { sha: string } {
  const object = responseObject(value, 'commit');
  const sha = responseString(object.sha, 'commit.sha');
  if (!/^(?:[a-f0-9]{40}|[a-f0-9]{64})$/i.test(sha)) invalidResponse('commit.sha', 'is invalid');
  return { sha };
}

function parseContent(value: unknown, source: string): { content: string; encoding: string } {
  const object = responseObject(value, source);
  return {
    content: responseString(object.content, `${source}.content`),
    encoding: responseString(object.encoding, `${source}.encoding`),
  };
}

function parseIssueList(value: unknown): IssueResponse[] {
  return responseArray(value, 'issue list').map((entry, index) => {
    const source = `issue list[${index}]`;
    const object = responseObject(entry, source);
    const labels = responseArray(object.labels, `${source}.labels`).map((label, labelIndex) => {
      const labelObject = responseObject(label, `${source}.labels[${labelIndex}]`);
      return { name: responseString(labelObject.name, `${source}.labels[${labelIndex}].name`) };
    });
    const updatedAt = responseString(object.updatedAt, `${source}.updatedAt`);
    if (!Number.isFinite(Date.parse(updatedAt))) invalidResponse(`${source}.updatedAt`, 'is invalid');
    return {
      number: positiveInteger(object.number, `${source}.number`),
      title: responseString(object.title, `${source}.title`, true),
      url: responseString(object.url, `${source}.url`),
      labels,
      updatedAt,
    };
  });
}

function parseIssueNumbers(value: unknown): Array<{ number: number }> {
  return responseArray(value, 'active issue list').map((entry, index) => {
    const object = responseObject(entry, `active issue list[${index}]`);
    return { number: positiveInteger(object.number, `active issue list[${index}].number`) };
  });
}

function parseClassicProtection(value: unknown): ClassicProtection {
  const root = responseObject(value, 'classic branch protection');
  const status = optionalObject(root.required_status_checks, 'required_status_checks');
  const reviews = optionalObject(
    root.required_pull_request_reviews,
    'required_pull_request_reviews',
  );
  const bypassAllowances = optionalObject(
    reviews?.bypass_pull_request_allowances,
    'required_pull_request_reviews.bypass_pull_request_allowances',
  );
  return {
    ...(status === undefined
      ? {}
      : {
          required_status_checks: {
            strict: optionalBoolean(status.strict, 'required_status_checks.strict'),
            contexts: optionalStringArray(status.contexts, 'required_status_checks.contexts'),
            checks: optionalArray(status.checks, 'required_status_checks.checks')?.map(
              (entry, index) => {
                const check = responseObject(entry, `required_status_checks.checks[${index}]`);
                return {
                  context: responseString(
                    check.context,
                    `required_status_checks.checks[${index}].context`,
                  ),
                  app_id: optionalNullableInteger(
                    check.app_id,
                    `required_status_checks.checks[${index}].app_id`,
                  ),
                };
              },
            ),
          },
        }),
    ...(reviews === undefined
      ? {}
      : {
          required_pull_request_reviews: {
            dismiss_stale_reviews: optionalBoolean(
              reviews.dismiss_stale_reviews,
              'required_pull_request_reviews.dismiss_stale_reviews',
            ),
            required_approving_review_count: optionalInteger(
              reviews.required_approving_review_count,
              'required_pull_request_reviews.required_approving_review_count',
            ),
            bypass_pull_request_allowances: {
              apps:
                optionalArray(
                  bypassAllowances?.apps,
                  'required_pull_request_reviews.bypass_pull_request_allowances.apps',
                )?.map((entry, index) => {
                  const app = responseObject(
                    entry,
                    `required_pull_request_reviews.bypass_pull_request_allowances.apps[${index}]`,
                  );
                  return {
                    id: positiveInteger(
                      app.id,
                      `required_pull_request_reviews.bypass_pull_request_allowances.apps[${index}].id`,
                    ),
                  };
                }) ?? [],
            },
          },
        }),
  };
}

function parseActiveRules(value: unknown): ActiveRule[] {
  return responseArray(value, 'active branch rules').map((entry, index) => {
    const source = `active branch rules[${index}]`;
    const object = responseObject(entry, source);
    const parameters = optionalObject(object.parameters, `${source}.parameters`);
    return {
      type: responseString(object.type, `${source}.type`),
      ruleset_id: optionalInteger(object.ruleset_id, `${source}.ruleset_id`),
      ...(parameters === undefined
        ? {}
        : {
            parameters: {
              strict_required_status_checks_policy: optionalBoolean(
                parameters.strict_required_status_checks_policy,
                `${source}.parameters.strict_required_status_checks_policy`,
              ),
              dismiss_stale_reviews_on_push: optionalBoolean(
                parameters.dismiss_stale_reviews_on_push,
                `${source}.parameters.dismiss_stale_reviews_on_push`,
              ),
              required_approving_review_count: optionalInteger(
                parameters.required_approving_review_count,
                `${source}.parameters.required_approving_review_count`,
              ),
              required_status_checks: optionalArray(
                parameters.required_status_checks,
                `${source}.parameters.required_status_checks`,
              )?.map((checkValue, checkIndex) => {
                const check = responseObject(
                  checkValue,
                  `${source}.parameters.required_status_checks[${checkIndex}]`,
                );
                return {
                  context: responseString(
                    check.context,
                    `${source}.parameters.required_status_checks[${checkIndex}].context`,
                  ),
                  integration_id: optionalNullableInteger(
                    check.integration_id,
                    `${source}.parameters.required_status_checks[${checkIndex}].integration_id`,
                  ),
                };
              }),
            },
          }),
    };
  });
}

function parseRulesetDetail(value: unknown): {
  bypassActors: Array<{ actorId: number | null; actorType: string }> | undefined;
} {
  const object = responseObject(value, 'ruleset detail');
  if (!Object.hasOwn(object, 'bypass_actors')) return { bypassActors: undefined };
  const bypassActors = responseArray(object.bypass_actors, 'ruleset detail.bypass_actors').map(
    (entry, index) => {
      const actor = responseObject(entry, `ruleset detail.bypass_actors[${index}]`);
      const actorId = optionalNullableInteger(
        actor.actor_id,
        `ruleset detail.bypass_actors[${index}].actor_id`,
      );
      return {
        actorId: actorId ?? null,
        actorType: responseString(
          actor.actor_type,
          `ruleset detail.bypass_actors[${index}].actor_type`,
        ),
      };
    },
  );
  return { bypassActors };
}

function responseObject(value: unknown, source: string): Record<string, unknown> {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    invalidResponse(source, 'must be an object');
  }
  return value as Record<string, unknown>;
}

function responseArray(value: unknown, source: string): unknown[] {
  if (!Array.isArray(value)) invalidResponse(source, 'must be an array');
  return value;
}

function optionalArray(value: unknown, source: string): unknown[] | undefined {
  return value === undefined || value === null ? undefined : responseArray(value, source);
}

function optionalObject(
  value: unknown,
  source: string,
): Record<string, unknown> | undefined {
  return value === undefined || value === null ? undefined : responseObject(value, source);
}

function responseString(value: unknown, source: string, allowEmpty = false): string {
  if (typeof value !== 'string' || (!allowEmpty && value.length === 0)) {
    invalidResponse(source, 'must be a string');
  }
  return value;
}

function repositoryName(value: unknown, source: string): string {
  const name = responseString(value, source);
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(name)) invalidResponse(source, 'is invalid');
  return name;
}

function positiveInteger(value: unknown, source: string): number {
  if (!Number.isSafeInteger(value) || Number(value) <= 0) invalidResponse(source, 'must be positive');
  return Number(value);
}

function optionalInteger(value: unknown, source: string): number | undefined {
  if (value === undefined || value === null) return undefined;
  if (!Number.isSafeInteger(value) || Number(value) < 0) invalidResponse(source, 'must be non-negative');
  return Number(value);
}

function optionalNullableInteger(value: unknown, source: string): number | null | undefined {
  if (value === undefined) return undefined;
  if (value === null) return null;
  return optionalInteger(value, source);
}

function optionalBoolean(value: unknown, source: string): boolean | undefined {
  if (value === undefined || value === null) return undefined;
  if (typeof value !== 'boolean') invalidResponse(source, 'must be boolean');
  return value;
}

function optionalStringArray(value: unknown, source: string): string[] | undefined {
  return optionalArray(value, source)?.map((entry, index) =>
    responseString(entry, `${source}[${index}]`),
  );
}

function invalidResponse(source: string, detail: string): never {
  throw new EuphoError('invalid_github_response', `${source} ${detail}`);
}

function parseJson<T>(text: string, source: string): T {
  try {
    return JSON.parse(text) as T;
  } catch (error) {
    throw new EuphoError('invalid_github_response', `${source} returned invalid JSON`, 1, {
      cause: error,
    });
  }
}

function assertRepository(repository: string): void {
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository)) {
    throw new EuphoError('invalid_repository', `Expected OWNER/REPOSITORY, received ${repository}`);
  }
}

function isNotFound(error: unknown): boolean {
  if (!(error instanceof EuphoError)) return false;
  return /HTTP 404|status code 404|Not Found/i.test(error.message);
}
