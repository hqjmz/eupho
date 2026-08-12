import { createHash, createHmac, timingSafeEqual } from 'node:crypto';
import { join } from 'node:path';

import { EuphoError } from '../errors.js';
import { atomicWriteJson, readJsonFile } from '../infra/atomic-file.js';
import { canonicalJson } from './canonical-json.js';

export interface SignedEnvelope<T> {
  scheme: 'hmac-sha256';
  keyId: string;
  payload: T;
  mac: string;
}

export interface RevisionAnchor {
  schemaVersion: 1;
  runId: string;
  revision: number;
  payloadDigest: string;
  keyId: string;
  confirmed: boolean;
}

export function signEnvelope<T>(payload: T, keyId: string, key: Uint8Array): SignedEnvelope<T> {
  const body = signingBody(payload);
  return {
    scheme: 'hmac-sha256',
    keyId,
    payload,
    mac: createHmac('sha256', key).update(body, 'utf8').digest('hex'),
  };
}

export function verifyEnvelope<T>(
  envelope: SignedEnvelope<T>,
  keys: ReadonlyMap<string, Uint8Array>,
): T {
  if (envelope.scheme !== 'hmac-sha256') {
    throw new EuphoError('unsupported_signature', `Unsupported signature scheme ${envelope.scheme}`);
  }
  const key = keys.get(envelope.keyId);
  if (key === undefined) throw new EuphoError('unknown_signing_key', `Unknown key ${envelope.keyId}`);
  const expected = createHmac('sha256', key).update(signingBody(envelope.payload), 'utf8').digest();
  const actual = Buffer.from(envelope.mac, 'hex');
  if (actual.length !== expected.length || !timingSafeEqual(actual, expected)) {
    throw new EuphoError('invalid_signature', 'Signed metadata failed HMAC verification');
  }
  return envelope.payload;
}

export function envelopePayloadDigest(payload: unknown): string {
  return createHash('sha256').update(canonicalJson(payload), 'utf8').digest('hex');
}

export class RevisionLedger {
  constructor(private readonly stateRoot: string) {}

  async assertFresh(runId: string, revision: number, payloadDigest: string): Promise<void> {
    validateRevisionInput(runId, revision, payloadDigest);
    const current = await this.read(runId);
    if (current === null) return;
    if (revision < current.revision) {
      throw new EuphoError(
        'metadata_rollback',
        `Run ${runId} revision ${revision} is below local high-water mark ${current.revision}`,
      );
    }
    if (revision === current.revision && payloadDigest !== current.payloadDigest) {
      throw new EuphoError(
        'metadata_fork',
        `Run ${runId} revision ${revision} has a different payload digest`,
      );
    }
  }

  async prepare(anchor: Omit<RevisionAnchor, 'schemaVersion' | 'confirmed'>): Promise<void> {
    await this.assertFresh(anchor.runId, anchor.revision, anchor.payloadDigest);
    validateKeyId(anchor.keyId);
    const current = await this.read(anchor.runId);
    if (
      current !== null &&
      current.revision === anchor.revision &&
      current.keyId !== anchor.keyId
    ) {
      throw new EuphoError(
        'metadata_fork',
        `Run ${anchor.runId} revision ${anchor.revision} changed signing key without advancing`,
      );
    }
    await atomicWriteJson(this.pathFor(anchor.runId), {
      schemaVersion: 1,
      ...anchor,
      confirmed: false,
    } satisfies RevisionAnchor);
  }

  async confirm(runId: string, revision: number, payloadDigest: string): Promise<void> {
    validateRevisionInput(runId, revision, payloadDigest);
    const current = await this.read(runId);
    if (
      current === null ||
      current.revision !== revision ||
      current.payloadDigest !== payloadDigest
    ) {
      throw new EuphoError('revision_confirmation_mismatch', `Cannot confirm ${runId} revision ${revision}`);
    }
    await atomicWriteJson(this.pathFor(runId), { ...current, confirmed: true });
  }

  async read(runId: string): Promise<RevisionAnchor | null> {
    try {
      const value = await readJsonFile<RevisionAnchor>(this.pathFor(runId));
      if (
        value.schemaVersion !== 1 ||
        value.runId !== runId ||
        !Number.isSafeInteger(value.revision) ||
        value.revision < 1 ||
        typeof value.payloadDigest !== 'string' ||
        !/^[a-f0-9]{64}$/.test(value.payloadDigest) ||
        typeof value.keyId !== 'string' ||
        !/^[A-Za-z0-9._-]{1,128}$/.test(value.keyId) ||
        typeof value.confirmed !== 'boolean'
      ) {
        throw new EuphoError('invalid_revision_anchor', `Invalid revision anchor for ${runId}`);
      }
      return value;
    } catch (error) {
      if (error instanceof EuphoError && error.cause instanceof Error && 'code' in error.cause) {
        if ((error.cause as NodeJS.ErrnoException).code === 'ENOENT') return null;
      }
      throw error;
    }
  }

  private pathFor(runId: string): string {
    if (!/^[A-Za-z0-9._-]+$/.test(runId)) throw new EuphoError('invalid_run_id', `Unsafe run ID ${runId}`);
    return join(this.stateRoot, 'revisions', `${runId}.json`);
  }
}

function validateRevisionInput(runId: string, revision: number, payloadDigest: string): void {
  if (!/^[A-Za-z0-9._-]+$/.test(runId)) {
    throw new EuphoError('invalid_run_id', `Unsafe run ID ${runId}`);
  }
  if (!Number.isSafeInteger(revision) || revision < 1) {
    throw new EuphoError('invalid_revision', `Invalid revision ${revision} for ${runId}`);
  }
  if (!/^[a-f0-9]{64}$/.test(payloadDigest)) {
    throw new EuphoError('invalid_payload_digest', `Invalid payload digest for ${runId}`);
  }
}

function validateKeyId(keyId: string): void {
  if (!/^[A-Za-z0-9._-]{1,128}$/.test(keyId)) {
    throw new EuphoError('invalid_key_id', `Unsafe signing key ID ${keyId}`);
  }
}

function signingBody(payload: unknown): string {
  return `eupho:v1\n${canonicalJson(payload)}`;
}
