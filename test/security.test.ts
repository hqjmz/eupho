import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';

import { EuphoError } from '../src/errors.js';
import { canonicalDigest, canonicalJson } from '../src/security/canonical-json.js';
import {
  envelopePayloadDigest,
  RevisionLedger,
  signEnvelope,
  verifyEnvelope,
  type SignedEnvelope,
} from '../src/security/signed-metadata.js';

test('canonical JSON sorts object keys recursively and preserves array order', () => {
  const value = {
    z: [{ y: 2, x: 1 }, 'last'],
    a: { beta: true, alpha: null },
  };

  assert.equal(
    canonicalJson(value),
    '{"a":{"alpha":null,"beta":true},"z":[{"x":1,"y":2},"last"]}',
  );
  assert.equal(
    canonicalDigest(value),
    canonicalDigest({ a: { alpha: null, beta: true }, z: [{ x: 1, y: 2 }, 'last'] }),
  );
});

test('canonical JSON fails closed for values JSON cannot represent safely', () => {
  const circular: { self?: unknown } = {};
  circular.self = circular;
  const sparse = new Array(1);
  for (const value of [
    Number.NaN,
    Number.POSITIVE_INFINITY,
    { missing: undefined },
    Symbol('x'),
    new Date(),
    '\ud800',
    circular,
    sparse,
  ]) {
    assert.throws(
      () => canonicalJson(value),
      (error: unknown) => isEuphoError(error, 'non_canonical_value'),
    );
  }
});

test('signed metadata verifies across key order and rejects payload tampering', () => {
  const key = Buffer.from('test-only-signing-secret', 'utf8');
  const envelope = signEnvelope({ z: 3, nested: { b: 2, a: 1 } }, 'active', key);

  assert.deepEqual(verifyEnvelope(envelope, new Map([['active', key]])), envelope.payload);
  assert.equal(
    envelope.mac,
    signEnvelope({ nested: { a: 1, b: 2 }, z: 3 }, 'active', key).mac,
  );

  const tampered: SignedEnvelope<{ z: number; nested: { b: number; a: number } }> = {
    ...envelope,
    payload: { ...envelope.payload, z: 4 },
  };
  assert.throws(
    () => verifyEnvelope(tampered, new Map([['active', key]])),
    (error: unknown) => isEuphoError(error, 'invalid_signature'),
  );
});

test('signed metadata rejects unknown signing keys and malformed MACs', () => {
  const key = Buffer.from('test-only-signing-secret', 'utf8');
  const envelope = signEnvelope({ runId: 'run-7', revision: 1 }, 'retired', key);

  assert.throws(
    () => verifyEnvelope(envelope, new Map()),
    (error: unknown) => isEuphoError(error, 'unknown_signing_key'),
  );
  assert.throws(
    () => verifyEnvelope({ ...envelope, mac: 'not-hex' }, new Map([['retired', key]])),
    (error: unknown) => isEuphoError(error, 'invalid_signature'),
  );
});

test('payload digests are stable for semantically identical object ordering', () => {
  assert.equal(
    envelopePayloadDigest({ repository: 'acme/widgets', revision: 2 }),
    envelopePayloadDigest({ revision: 2, repository: 'acme/widgets' }),
  );
});

test('revision ledger persists prepare/confirm state and rejects rollback or fork', async () => {
  await withTempDirectory('eupho-ledger-', async (stateRoot) => {
    const ledger = new RevisionLedger(stateRoot);
    const digestOne = envelopePayloadDigest({ revision: 1, status: 'ready' });

    assert.equal(await ledger.read('run-123'), null);
    await ledger.prepare({
      runId: 'run-123',
      revision: 1,
      payloadDigest: digestOne,
      keyId: 'active',
    });
    assert.deepEqual(await ledger.read('run-123'), {
      schemaVersion: 1,
      runId: 'run-123',
      revision: 1,
      payloadDigest: digestOne,
      keyId: 'active',
      confirmed: false,
    });

    await ledger.assertFresh('run-123', 1, digestOne);
    await assert.rejects(
      ledger.assertFresh('run-123', 0, digestOne),
      (error: unknown) => isEuphoError(error, 'invalid_revision'),
    );
    await assert.rejects(
      ledger.prepare({
        runId: 'run-123',
        revision: 1,
        payloadDigest: digestOne,
        keyId: 'different-key',
      }),
      (error: unknown) => isEuphoError(error, 'metadata_fork'),
    );
    await assert.rejects(
      ledger.assertFresh('run-123', 1, envelopePayloadDigest({ revision: 1, status: 'changed' })),
      (error: unknown) => isEuphoError(error, 'metadata_fork'),
    );
    await assert.rejects(
      ledger.confirm('run-123', 2, digestOne),
      (error: unknown) => isEuphoError(error, 'revision_confirmation_mismatch'),
    );

    await ledger.confirm('run-123', 1, digestOne);
    assert.equal((await ledger.read('run-123'))?.confirmed, true);

    const digestTwo = envelopePayloadDigest({ revision: 2, status: 'claimed' });
    await ledger.prepare({
      runId: 'run-123',
      revision: 2,
      payloadDigest: digestTwo,
      keyId: 'next',
    });
    assert.equal((await ledger.read('run-123'))?.revision, 2);
    assert.equal((await ledger.read('run-123'))?.confirmed, false);
    await assert.rejects(
      ledger.assertFresh('run-123', 1, digestOne),
      (error: unknown) => isEuphoError(error, 'metadata_rollback'),
    );
  });
});

test('revision ledger rejects run identifiers that could escape its state directory', async () => {
  await withTempDirectory('eupho-ledger-id-', async (stateRoot) => {
    const ledger = new RevisionLedger(stateRoot);
    await assert.rejects(
      ledger.read('../outside'),
      (error: unknown) => isEuphoError(error, 'invalid_run_id'),
    );
    await assert.rejects(
      ledger.prepare({
        runId: 'valid-run',
        revision: 0,
        payloadDigest: '0'.repeat(64),
        keyId: 'active',
      }),
      (error: unknown) => isEuphoError(error, 'invalid_revision'),
    );
  });
});

function isEuphoError(error: unknown, code: string): boolean {
  return error instanceof EuphoError && error.code === code;
}

async function withTempDirectory(
  prefix: string,
  operation: (path: string) => Promise<void>,
): Promise<void> {
  const path = await mkdtemp(join(tmpdir(), prefix));
  try {
    await operation(path);
  } finally {
    await rm(path, { recursive: true, force: true });
  }
}
