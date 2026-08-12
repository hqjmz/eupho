import assert from 'node:assert/strict';
import test from 'node:test';

import { canTransition, transition } from '../src/domain/state-machine.js';
import type { RunRecord } from '../src/domain/run.js';
import { issueLabelProjection } from '../src/domain/run.js';
import { EuphoError } from '../src/errors.js';

const initialTime = '2026-08-12T00:00:00.000Z';

function run(overrides: Partial<RunRecord> = {}): RunRecord {
  return {
    schemaVersion: 1,
    revision: 0,
    runId: 'run-001',
    repositoryId: 4242,
    repository: 'example/eupho-target',
    issueNumber: 17,
    state: 'ready',
    phase: 'intake',
    executionMode: 'attended',
    workspaceType: 'worktree',
    mergePolicy: 'human-final-approval',
    baseBranch: 'main',
    baseSha: '1111111111111111111111111111111111111111',
    branch: null,
    pullRequest: null,
    headSha: null,
    reviewBinding: null,
    attempts: { author: 0, validation: 0, review: 0, repair: 0 },
    usage: { modelTokens: 0, costUsd: '0' },
    attentionCode: null,
    attentionReason: null,
    resumePhase: null,
    createdAt: initialTime,
    updatedAt: initialTime,
    ...overrides,
  };
}

function at(second: number): string {
  return `2026-08-12T00:00:${String(second).padStart(2, '0')}.000Z`;
}

function assertInvalidTransition(error: unknown, expectedMessage: RegExp): boolean {
  assert.ok(error instanceof EuphoError);
  assert.equal(error.code, 'invalid_transition');
  assert.match(error.message, expectedMessage);
  return true;
}

test('human-approval lifecycle advances through author, validation, review, and merge', () => {
  let current = run();

  current = transition(current, { type: 'claim', at: at(1) });
  assert.deepEqual([current.state, current.phase, current.revision], ['work_in_progress', 'author', 1]);

  current = transition(current, { type: 'author_completed', at: at(2) });
  assert.deepEqual([current.state, current.phase, current.revision], [
    'work_in_progress',
    'validation',
    2,
  ]);

  current = transition(current, { type: 'validation_completed', at: at(3) });
  assert.deepEqual([current.state, current.phase], ['in_review', 'review']);

  current = transition(current, { type: 'review_clean_requires_approval', at: at(4) });
  assert.deepEqual([current.state, current.phase], ['in_review', 'awaiting_approval']);
  assert.equal(issueLabelProjection(current.state, current.phase), 'review');

  current = transition(current, { type: 'approval_recorded', at: at(5) });
  assert.deepEqual([current.state, current.phase], ['in_review', 'awaiting_approval']);

  current = transition(current, { type: 'merged', at: at(6) });
  assert.deepEqual([current.state, current.phase, current.revision], ['merged', 'complete', 6]);
  assert.equal(current.updatedAt, at(6));
  assert.equal(issueLabelProjection(current.state, current.phase), null);
  assert.equal(canTransition(current, 'claim'), false);
});

test('review findings enter repair and must return through validation', () => {
  let current = run({ state: 'in_review', phase: 'review', revision: 3 });

  current = transition(current, { type: 'review_has_findings', at: at(4) });
  assert.deepEqual([current.state, current.phase], ['in_review', 'repair']);

  current = transition(current, { type: 'author_completed', at: at(5) });
  assert.deepEqual([current.state, current.phase], ['work_in_progress', 'validation']);

  current = transition(current, { type: 'validation_completed', at: at(6) });
  assert.deepEqual([current.state, current.phase], ['in_review', 'review']);
});

test('escalation preserves a safe resume point and resume clears attention state', () => {
  const validating = run({ state: 'work_in_progress', phase: 'validation', revision: 2 });
  const paused = transition(validating, {
    type: 'escalate',
    at: at(3),
    reason: 'validation command needs permission',
  });

  assert.equal(paused.state, 'needs_human');
  assert.equal(paused.phase, 'paused');
  assert.equal(paused.resumePhase, 'validation');
  assert.equal(paused.attentionCode, 'escalation');
  assert.equal(paused.attentionReason, 'validation command needs permission');
  assert.equal(issueLabelProjection(paused.state, paused.phase), 'human');

  const resumed = transition(paused, { type: 'resume', at: at(4) });
  assert.equal(resumed.state, 'work_in_progress');
  assert.equal(resumed.phase, 'validation');
  assert.equal(resumed.resumePhase, null);
  assert.equal(resumed.attentionCode, null);
  assert.equal(resumed.attentionReason, null);
  assert.equal(issueLabelProjection(resumed.state, resumed.phase), 'working');
});

test('review escalation resumes in review, while no-change requires an explicit terminal decision', () => {
  const reviewing = run({ state: 'in_review', phase: 'review' });
  const pausedReview = transition(reviewing, { type: 'external_change', at: at(1) });
  const resumedReview = transition(pausedReview, { type: 'resume', at: at(2) });
  assert.deepEqual([resumedReview.state, resumedReview.phase], ['in_review', 'review']);

  const authoring = run({ state: 'work_in_progress', phase: 'author' });
  const noChange = transition(authoring, { type: 'no_change', at: at(1) });
  assert.deepEqual([noChange.state, noChange.phase], ['needs_human', 'paused']);
  assert.equal(noChange.attentionReason, 'no_change');
  assert.equal(noChange.attentionCode, 'no_change');

  const accepted = transition(noChange, { type: 'accept_no_change', at: at(2) });
  assert.deepEqual([accepted.state, accepted.phase], ['completed_no_change', 'complete']);
  assert.equal(issueLabelProjection(accepted.state, accepted.phase), null);
});

test('invalid transitions fail closed and do not mutate the source record', () => {
  const original = run();

  assert.equal(canTransition(original, 'merged'), false);
  assert.throws(
    () => transition(original, { type: 'merged', at: at(1) }),
    (error: unknown) => assertInvalidTransition(error, /Cannot apply merged.*ready\/intake/),
  );
  assert.deepEqual(original, run());

  const reviewing = run({ state: 'in_review', phase: 'review' });
  assert.equal(canTransition(reviewing, 'approval_recorded'), false);
  assert.throws(
    () => transition(reviewing, { type: 'approval_recorded', at: at(1) }),
    (error: unknown) =>
      assertInvalidTransition(error, /Cannot apply approval_recorded.*in_review\/review/),
  );
});

test('phase guards prevent skipping authoring, validation, or repair', () => {
  const authoring = run({ state: 'work_in_progress', phase: 'author' });
  assert.equal(canTransition(authoring, 'validation_completed'), false);
  assert.throws(
    () => transition(authoring, { type: 'validation_completed', at: at(1) }),
    (error: unknown) => assertInvalidTransition(error, /work_in_progress\/author/),
  );

  const repairing = run({ state: 'in_review', phase: 'repair' });
  assert.equal(canTransition(repairing, 'review_clean_autonomous'), false);
  assert.throws(
    () => transition(repairing, { type: 'review_clean_autonomous', at: at(1) }),
    (error: unknown) => assertInvalidTransition(error, /in_review\/repair/),
  );
});

test('merge-policy guards keep autonomous and human approval paths disjoint', () => {
  const humanReview = run({ state: 'in_review', phase: 'review' });
  assert.equal(canTransition(humanReview, 'review_clean_autonomous'), false);
  assert.throws(
    () => transition(humanReview, { type: 'review_clean_autonomous', at: at(1) }),
    (error: unknown) => assertInvalidTransition(error, /requires merge policy autonomous-low-risk/),
  );

  let autonomous = run({
    state: 'in_review',
    phase: 'review',
    mergePolicy: 'autonomous-low-risk',
    executionMode: 'unattended',
    workspaceType: 'ephemeral_clone',
  });
  assert.equal(canTransition(autonomous, 'review_clean_requires_approval'), false);
  autonomous = transition(autonomous, { type: 'review_clean_autonomous', at: at(1) });
  assert.deepEqual([autonomous.state, autonomous.phase], ['in_review', 'merge_wait']);
  autonomous = transition(autonomous, { type: 'merged', at: at(2) });
  assert.deepEqual([autonomous.state, autonomous.phase], ['merged', 'complete']);

  const suggestOnly = run({ state: 'in_review', phase: 'review', mergePolicy: 'suggest-only' });
  assert.equal(canTransition(suggestOnly, 'review_clean_autonomous'), false);
  assert.equal(canTransition(suggestOnly, 'review_clean_requires_approval'), false);
  let handedOff = transition(suggestOnly, { type: 'review_clean_suggest_only', at: at(3) });
  assert.deepEqual([handedOff.state, handedOff.phase], ['in_review', 'merge_wait']);
  handedOff = transition(handedOff, { type: 'merged', at: at(4) });
  assert.deepEqual([handedOff.state, handedOff.phase], ['merged', 'complete']);
});

test('no-change acceptance is reason-bound and base changes require revalidation', () => {
  const unrelatedPause = run({
    state: 'needs_human',
    phase: 'paused',
    attentionCode: 'escalation',
    attentionReason: 'protected_path',
    resumePhase: 'review',
  });
  assert.equal(canTransition(unrelatedPause, 'accept_no_change'), false);

  const awaiting = run({ state: 'in_review', phase: 'awaiting_approval' });
  const rebound = transition(awaiting, { type: 'binding_changed', at: at(1) });
  assert.deepEqual([rebound.state, rebound.phase], ['work_in_progress', 'validation']);
});

test('no-change authorization is derived from the transition, never free-form text', () => {
  const spoofed = transition(
    run({ state: 'work_in_progress', phase: 'author' }),
    { type: 'escalate', at: at(1), reason: 'no_change' },
  );
  assert.equal(spoofed.attentionCode, 'escalation');
  assert.equal(spoofed.attentionReason, 'no_change');
  assert.equal(canTransition(spoofed, 'accept_no_change'), false);

  const verified = transition(
    run({ state: 'work_in_progress', phase: 'author' }),
    { type: 'no_change', at: at(2), reason: 'Verified empty diff at current base' },
  );
  assert.equal(verified.attentionCode, 'no_change');
  assert.equal(verified.attentionReason, 'Verified empty diff at current base');
  assert.equal(canTransition(verified, 'accept_no_change'), true);
});
