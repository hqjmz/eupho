import { execFile } from 'node:child_process';
import { promisify } from 'node:util';

import { findRepositoryConfig, loadHostConfig, loadRepositoryConfig } from '../config/load.js';
import type { HostConfig, RepositoryConfig } from '../config/types.js';
import { messageOf } from '../errors.js';
import { GhReader } from '../github/gh-reader.js';
import type { BranchPolicySnapshot } from '../github/types.js';
import { resolvePolicy } from './policy.js';

const execFileAsync = promisify(execFile);

export type DiagnosticStatus = 'pass' | 'fail' | 'warn' | 'skip';

export interface Diagnostic {
  code: string;
  status: DiagnosticStatus;
  message: string;
  remediation: string | null;
}

export interface DoctorReport {
  ok: boolean;
  repository: string | null;
  policySource: string | null;
  checks: Diagnostic[];
}

export async function doctor(options: {
  cwd: string;
  repository?: string;
  configPath?: string;
  hostConfigPath?: string;
  environment?: NodeJS.ProcessEnv;
}): Promise<DoctorReport> {
  const environment = options.environment ?? process.env;
  const checks: Diagnostic[] = [];

  checks.push(
    process.versions.node.split('.')[0] !== undefined && Number(process.versions.node.split('.')[0]) >= 22
      ? pass('runtime.node', `Node ${process.versions.node} satisfies >=22`)
      : fail('runtime.node', `Node ${process.versions.node} is unsupported`, 'Install Node 22 or newer.'),
  );
  checks.push(await executableCheck('runtime.git', 'git'));
  checks.push(await executableCheck('runtime.gh', 'gh'));

  let hostConfig: HostConfig | null = null;
  if (options.hostConfigPath !== undefined) {
    try {
      hostConfig = (await loadHostConfig(options.hostConfigPath)).value;
      checks.push(pass('config.host', `Loaded host configuration from ${options.hostConfigPath}`));
    } catch (error) {
      checks.push(fail('config.host', messageOf(error), 'Fix the administrator-owned host configuration.'));
    }
  } else {
    checks.push(
      warn(
        'config.host',
        'No host configuration supplied; local syntax checks can continue',
        'Pass --host-config before strict GitHub or unattended checks.',
      ),
    );
  }

  let repositoryConfig: RepositoryConfig | null = null;
  let policySource: string | null = null;

  if (options.repository === undefined) {
    try {
      const path = await findRepositoryConfig(options.cwd, options.configPath);
      const loaded = await loadRepositoryConfig(path);
      repositoryConfig = loaded.value;
      policySource = loaded.source;
      checks.push(pass('config.repository', `Loaded repository policy from ${loaded.source}`));
    } catch (error) {
      checks.push(fail('config.repository', messageOf(error), 'Fix or supply the repository policy.'));
    }
  } else {
    const token = environment.EUPHO_DOCTOR_TOKEN;
    if (token === undefined || token.length === 0) {
      checks.push(
        fail(
          'github.doctor_token',
          'EUPHO_DOCTOR_TOKEN is required for strict GitHub diagnostics',
          'Supply a separate operator token with repository read and Administration:read access.',
        ),
      );
    } else {
      checks.push(pass('github.doctor_token', 'Separate operator diagnostic credential is present'));
      try {
        const reader = new GhReader({ token, environment });
        const resolved = await resolvePolicy({
          reader,
          repository: options.repository,
          cwd: options.cwd,
          ...(options.configPath === undefined ? {} : { explicitConfig: options.configPath }),
        });
        repositoryConfig = resolved.config;
        policySource = resolved.source;
        checks.push(
          resolved.trustedBase
            ? pass('config.repository', `Loaded trusted base policy from ${resolved.source}`)
            : warn(
                'config.repository',
                `Using local policy override ${resolved.source}`,
                'Omit --config for a production preflight against the trusted base SHA.',
              ),
        );
        for (const [role, label] of Object.entries(resolved.config.labels)) {
          checks.push(
            (await reader.labelExists(options.repository, label))
              ? pass(`github.label.${role}`, `${role} label ${label} exists`)
              : fail(
                  `github.label.${role}`,
                  `${role} label ${label} does not exist`,
                  'Create every configured workflow label before dispatch.',
                ),
          );
        }

        if (hostConfig === null) {
          checks.push(
            fail(
              'github.branch_policy',
              'Host configuration is required to verify the expected GitHub App source',
              'Pass --host-config with the installed App ID.',
            ),
          );
        } else {
          const branchPolicy = await reader.branchPolicy(options.repository, resolved.config.baseBranch);
          checks.push(...evaluateBranchPolicy(branchPolicy, resolved.config, hostConfig.githubApp.appId));
        }
      } catch (error) {
        checks.push(fail('github.read', messageOf(error), 'Verify repository access and diagnostic permissions.'));
      }
    }
  }

  if (repositoryConfig !== null) {
    checks.push(...evaluateRepositoryConfig(repositoryConfig, hostConfig));
  }

  return {
    ok: checks.every((check) => check.status !== 'fail'),
    repository: options.repository ?? null,
    policySource,
    checks,
  };
}

export function evaluateBranchPolicy(
  policy: BranchPolicySnapshot,
  config: RepositoryConfig,
  expectedAppId: number,
): Diagnostic[] {
  const checks: Diagnostic[] = [];
  checks.push(
    policy.sources.length > 0
      ? pass('github.protection_sources', `Evaluated ${policy.sources.join(' and ')}`)
      : fail(
          'github.protection_sources',
          'No active classic protection or repository ruleset was found',
          'Configure a protected branch or ruleset.',
        ),
  );
  checks.push(
    policy.strictRequiredChecks
      ? pass('github.strict_checks', 'Required checks use strict up-to-date policy')
      : fail(
          'github.strict_checks',
          'Required checks are not strict',
          'Require branches to be up to date before merging.',
        ),
  );

  const matchingCheck = policy.requiredChecks.find(
    (check) => check.context === config.review.requiredCheck && check.appId === expectedAppId,
  );
  checks.push(
    matchingCheck !== undefined
      ? pass(
          'github.expected_check_source',
          `${config.review.requiredCheck} is bound to App ${expectedAppId}`,
        )
      : fail(
          'github.expected_check_source',
          `${config.review.requiredCheck} is not bound to App ${expectedAppId}`,
          'Bind the required check to the installed Eupho GitHub App, not any source.',
        ),
  );
  checks.push(
    policy.bypassVerificationComplete
      ? pass('github.bypass_visibility', 'Ruleset and branch bypass actors were visible')
      : fail(
          'github.bypass_visibility',
          'Ruleset bypass actors were not visible to the diagnostic credential',
          'Use an operator credential whose owner can inspect the applicable rulesets.',
        ),
  );
  checks.push(
    !policy.bypassAppIds.includes(expectedAppId)
      ? pass('github.no_app_bypass', `App ${expectedAppId} has no configured merge bypass`)
      : fail(
          'github.no_app_bypass',
          `App ${expectedAppId} can bypass the protected merge path`,
          'Remove the Eupho App from branch-protection and ruleset bypass actors.',
        ),
  );

  if (config.mergePolicy === 'human-final-approval') {
    checks.push(
      policy.dismissStaleApprovals
        ? pass('github.stale_approvals', 'Stale approvals are dismissed on push')
        : fail(
            'github.stale_approvals',
            'Stale approvals are not dismissed on push',
            'Enable stale approval dismissal for human-final-approval.',
          ),
    );
    checks.push(
      policy.requiredApprovingReviewCount >= 1
        ? pass(
            'github.required_approval',
            `Branch policy requires ${policy.requiredApprovingReviewCount} approving review(s)`,
          )
        : fail(
            'github.required_approval',
            'Branch policy does not require an approving review',
            'Require at least one approving review for human-final-approval.',
          ),
    );
  }
  return checks;
}

function evaluateRepositoryConfig(config: RepositoryConfig, host: HostConfig | null): Diagnostic[] {
  const checks: Diagnostic[] = [];
  checks.push(
    config.branches.requireUpToDate
      ? pass('policy.strict_binding', 'Repository policy requires up-to-date branches')
      : fail(
          'policy.strict_binding',
          'Repository policy permits stale base bindings',
          'Set branches.require_up_to_date to true.',
        ),
  );
  checks.push(
    config.notifications.events.includes('awaiting_approval')
      ? pass('policy.approval_notification', 'awaiting_approval notifications are enabled')
      : fail(
          'policy.approval_notification',
          'awaiting_approval is missing from notification events',
          'Add awaiting_approval to notifications.events.',
        ),
  );

  if (host !== null) {
    const selectedProfiles = new Set([
      config.execution.unattended.sandboxProfile,
      ...config.routing.autonomousClasses.map(() => config.execution.unattended.sandboxProfile),
    ]);
    for (const profile of selectedProfiles) {
      checks.push(
        Object.hasOwn(host.sandboxProfiles, profile)
          ? pass('host.sandbox_profile', `Sandbox profile ${profile} exists`)
          : fail(
              'host.sandbox_profile',
              `Sandbox profile ${profile} is unknown`,
              'Predeclare the selected sandbox profile in host configuration.',
            ),
      );
    }
    checks.push(
      Object.hasOwn(host.workspaceProfiles, 'ephemeral_clone')
        ? pass('host.workspace_profile', 'Disposable ephemeral_clone profile exists')
        : fail(
            'host.workspace_profile',
            'Disposable ephemeral_clone profile is missing',
            'Declare an ephemeral_clone profile with no shared objects or authenticated remote.',
          ),
    );
    checks.push(
      Object.hasOwn(host.priceTableProfiles, config.limits.priceTableProfile)
        ? pass('host.price_table', `Price table profile ${config.limits.priceTableProfile} exists`)
        : fail(
            'host.price_table',
            `Price table profile ${config.limits.priceTableProfile} is unknown`,
            'Predeclare the selected price table profile in host configuration.',
          ),
    );
    for (const sink of config.notifications.sinks) {
      checks.push(
        Object.hasOwn(host.notificationSinks, sink)
          ? pass('host.notification_sink', `Notification sink ${sink} exists`)
          : fail(
              'host.notification_sink',
              `Notification sink ${sink} is unknown`,
              'Predeclare the notification sink in host configuration.',
            ),
      );
    }
  }
  return checks;
}

async function executableCheck(code: string, executable: string): Promise<Diagnostic> {
  try {
    const result = await execFileAsync(executable, ['--version'], { encoding: 'utf8' });
    const firstLine = result.stdout.trim().split('\n')[0] ?? executable;
    return pass(code, firstLine);
  } catch (error) {
    return fail(code, `${executable} is unavailable: ${messageOf(error)}`, `Install ${executable}.`);
  }
}

function pass(code: string, message: string): Diagnostic {
  return { code, status: 'pass', message, remediation: null };
}

function fail(code: string, message: string, remediation: string): Diagnostic {
  return { code, status: 'fail', message, remediation };
}

function warn(code: string, message: string, remediation: string): Diagnostic {
  return { code, status: 'warn', message, remediation };
}
