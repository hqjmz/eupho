import { EuphoError } from '../errors.js';
import type { MergePolicy, RunPhase, RunRecord, RunState } from './run.js';

export const RUN_EVENTS = [
  'claim',
  'author_completed',
  'no_change',
  'validation_completed',
  'review_has_findings',
  'review_clean_autonomous',
  'review_clean_requires_approval',
  'review_clean_suggest_only',
  'approval_recorded',
  'binding_changed',
  'merged',
  'escalate',
  'resume',
  'accept_no_change',
  'cancel',
  'external_change',
] as const;

export type RunEventType = (typeof RUN_EVENTS)[number];

export interface RunEvent {
  type: RunEventType;
  at: string;
  reason?: string;
}

interface TransitionTarget {
  state: RunState;
  phase: RunPhase;
}

type TransitionResolver = TransitionTarget | ((run: RunRecord) => TransitionTarget);
type LifecycleKey = `${RunState}/${RunPhase}`;

const PAUSED: TransitionTarget = { state: 'needs_human', phase: 'paused' };
const CANCELLED: TransitionTarget = { state: 'cancelled', phase: 'complete' };
const REVALIDATE: TransitionTarget = { state: 'work_in_progress', phase: 'validation' };

const transitions: Partial<
  Record<LifecycleKey, Partial<Record<RunEventType, TransitionResolver>>>
> = {
  'ready/intake': {
    claim: { state: 'work_in_progress', phase: 'author' },
    cancel: CANCELLED,
  },
  'work_in_progress/author': {
    author_completed: REVALIDATE,
    no_change: PAUSED,
    escalate: PAUSED,
    external_change: PAUSED,
    cancel: CANCELLED,
  },
  'work_in_progress/validation': {
    validation_completed: { state: 'in_review', phase: 'review' },
    no_change: PAUSED,
    escalate: PAUSED,
    external_change: PAUSED,
    cancel: CANCELLED,
  },
  'in_review/review': {
    review_has_findings: { state: 'in_review', phase: 'repair' },
    review_clean_autonomous: requirePolicy('autonomous-low-risk', {
      state: 'in_review',
      phase: 'merge_wait',
    }),
    review_clean_requires_approval: requirePolicy('human-final-approval', {
      state: 'in_review',
      phase: 'awaiting_approval',
    }),
    review_clean_suggest_only: requirePolicy('suggest-only', {
      state: 'in_review',
      phase: 'merge_wait',
    }),
    binding_changed: REVALIDATE,
    escalate: PAUSED,
    external_change: PAUSED,
    cancel: CANCELLED,
  },
  'in_review/repair': {
    author_completed: REVALIDATE,
    binding_changed: REVALIDATE,
    escalate: PAUSED,
    external_change: PAUSED,
    cancel: CANCELLED,
  },
  'in_review/awaiting_approval': {
    approval_recorded: requirePolicy('human-final-approval', {
      state: 'in_review',
      phase: 'awaiting_approval',
    }),
    binding_changed: REVALIDATE,
    merged: requirePolicy('human-final-approval', { state: 'merged', phase: 'complete' }),
    escalate: PAUSED,
    external_change: PAUSED,
    cancel: CANCELLED,
  },
  'in_review/merge_wait': {
    binding_changed: REVALIDATE,
    merged: requireOneOfPolicies(
      ['autonomous-low-risk', 'suggest-only'],
      { state: 'merged', phase: 'complete' },
    ),
    escalate: PAUSED,
    external_change: PAUSED,
    cancel: CANCELLED,
  },
  'needs_human/paused': {
    resume: resumeTarget,
    accept_no_change: acceptNoChange,
    cancel: CANCELLED,
  },
};

export function canTransition(run: RunRecord, event: RunEventType): boolean {
  const resolver = resolverFor(run, event);
  if (resolver === undefined) return false;
  try {
    if (typeof resolver === 'function') resolver(run);
    return true;
  } catch (error) {
    if (error instanceof EuphoError && error.code === 'invalid_transition') return false;
    throw error;
  }
}

export function transition(run: RunRecord, event: RunEvent): RunRecord {
  const resolver = resolverFor(run, event.type);
  if (resolver === undefined) invalidTransition(run, event.type);
  const target = typeof resolver === 'function' ? resolver(run) : resolver;
  const escalating = target.state === 'needs_human';
  const resuming = event.type === 'resume';

  return {
    ...run,
    revision: run.revision + 1,
    state: target.state,
    phase: target.phase,
    attentionCode: escalating
      ? attentionCodeFor(event.type)
      : resuming
        ? null
        : run.attentionCode,
    attentionReason: escalating ? (event.reason ?? event.type) : resuming ? null : run.attentionReason,
    resumePhase: escalating ? run.phase : resuming ? null : run.resumePhase,
    updatedAt: event.at,
  };
}

function resolverFor(run: RunRecord, event: RunEventType): TransitionResolver | undefined {
  return transitions[`${run.state}/${run.phase}`]?.[event];
}

function requirePolicy(
  expected: MergePolicy,
  target: TransitionTarget,
): (run: RunRecord) => TransitionTarget {
  return (run) => {
    if (run.mergePolicy !== expected) {
      throw new EuphoError(
        'invalid_transition',
        `${run.runId} requires merge policy ${expected}, not ${run.mergePolicy}`,
      );
    }
    return target;
  };
}

function requireOneOfPolicies(
  expected: readonly MergePolicy[],
  target: TransitionTarget,
): (run: RunRecord) => TransitionTarget {
  return (run) => {
    if (!expected.includes(run.mergePolicy)) {
      throw new EuphoError(
        'invalid_transition',
        `${run.runId} requires merge policy ${expected.join(' or ')}, not ${run.mergePolicy}`,
      );
    }
    return target;
  };
}

function resumeTarget(run: RunRecord): TransitionTarget {
  switch (run.resumePhase) {
    case 'author':
    case 'validation':
      return { state: 'work_in_progress', phase: run.resumePhase };
    case 'review':
    case 'repair':
    case 'awaiting_approval':
    case 'merge_wait':
      return { state: 'in_review', phase: run.resumePhase };
    default:
      throw new EuphoError(
        'invalid_transition',
        `Run ${run.runId} has no safe resume phase`,
      );
  }
}

function acceptNoChange(run: RunRecord): TransitionTarget {
  if (run.attentionCode !== 'no_change') {
    throw new EuphoError(
      'invalid_transition',
      `Run ${run.runId} is not awaiting no-change confirmation`,
    );
  }
  return { state: 'completed_no_change', phase: 'complete' };
}

function attentionCodeFor(
  event: RunEventType,
): 'no_change' | 'escalation' | 'external_change' {
  switch (event) {
    case 'no_change':
      return 'no_change';
    case 'external_change':
      return 'external_change';
    case 'escalate':
      return 'escalation';
    default:
      throw new EuphoError('invalid_transition', `${event} cannot create an attention state`);
  }
}

function invalidTransition(run: RunRecord, event: RunEventType): never {
  throw new EuphoError(
    'invalid_transition',
    `Cannot apply ${event} while run ${run.runId} is ${run.state}/${run.phase}`,
  );
}
