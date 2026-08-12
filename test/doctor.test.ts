import assert from 'node:assert/strict';
import test from 'node:test';

import { evaluateBranchPolicy } from '../src/cli/doctor.js';
import { loadRepositoryConfig } from '../src/config/load.js';

test('strict branch policy accepts only the configured check from the expected App', async () => {
  const config = (await loadRepositoryConfig('.github/eupho.yml')).value;
  const checks = evaluateBranchPolicy(
    {
      strictRequiredChecks: true,
      dismissStaleApprovals: true,
      requiredApprovingReviewCount: 1,
      bypassAppIds: [],
      bypassVerificationComplete: true,
      requiredChecks: [
        { context: 'agent-review', appId: 123456, source: 'ruleset' },
      ],
      sources: ['ruleset'],
    },
    config,
    123456,
  );

  assert.equal(checks.every((check) => check.status === 'pass'), true);
});

test('a same-named check from another source fails closed', async () => {
  const config = (await loadRepositoryConfig('.github/eupho.yml')).value;
  const checks = evaluateBranchPolicy(
    {
      strictRequiredChecks: true,
      dismissStaleApprovals: true,
      requiredApprovingReviewCount: 1,
      bypassAppIds: [],
      bypassVerificationComplete: true,
      requiredChecks: [
        { context: 'agent-review', appId: 999999, source: 'classic_protection' },
      ],
      sources: ['classic_protection'],
    },
    config,
    123456,
  );

  assert.equal(
    checks.find((check) => check.code === 'github.expected_check_source')?.status,
    'fail',
  );
});

test('strictness and stale-approval requirements are independently enforced', async () => {
  const config = (await loadRepositoryConfig('.github/eupho.yml')).value;
  const checks = evaluateBranchPolicy(
    {
      strictRequiredChecks: false,
      dismissStaleApprovals: false,
      requiredApprovingReviewCount: 0,
      bypassAppIds: [123456],
      bypassVerificationComplete: false,
      requiredChecks: [
        { context: 'agent-review', appId: 123456, source: 'ruleset' },
      ],
      sources: ['ruleset'],
    },
    config,
    123456,
  );

  assert.equal(checks.find((check) => check.code === 'github.strict_checks')?.status, 'fail');
  assert.equal(checks.find((check) => check.code === 'github.stale_approvals')?.status, 'fail');
  assert.equal(checks.find((check) => check.code === 'github.required_approval')?.status, 'fail');
  assert.equal(checks.find((check) => check.code === 'github.bypass_visibility')?.status, 'fail');
  assert.equal(checks.find((check) => check.code === 'github.no_app_bypass')?.status, 'fail');
});
