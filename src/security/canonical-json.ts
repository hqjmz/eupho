import { createHash } from 'node:crypto';

import { EuphoError } from '../errors.js';

export function canonicalJson(value: unknown): string {
  return serialize(value, new Set<object>());
}

export function canonicalDigest(value: unknown): string {
  return `sha256:${createHash('sha256').update(canonicalJson(value), 'utf8').digest('hex')}`;
}

function serialize(value: unknown, ancestors: Set<object>): string {
  if (value === null) return 'null';
  if (typeof value === 'string') {
    assertUnicodeScalarString(value);
    return JSON.stringify(value);
  }
  if (typeof value === 'boolean') return JSON.stringify(value);
  if (typeof value === 'number') {
    if (!Number.isFinite(value)) {
      throw new EuphoError('non_canonical_value', 'Non-finite numbers are forbidden');
    }
    return JSON.stringify(value);
  }
  if (typeof value !== 'object') {
    throw new EuphoError('non_canonical_value', `Unsupported canonical JSON value: ${typeof value}`);
  }
  if (ancestors.has(value)) {
    throw new EuphoError('non_canonical_value', 'Circular values are forbidden');
  }

  ancestors.add(value);
  try {
    if (Array.isArray(value)) {
      const entries: string[] = [];
      for (let index = 0; index < value.length; index += 1) {
        if (!(index in value)) {
          throw new EuphoError('non_canonical_value', `Sparse array entry at index ${index}`);
        }
        entries.push(serialize(value[index], ancestors));
      }
      return `[${entries.join(',')}]`;
    }

    const prototype = Object.getPrototypeOf(value);
    if (prototype !== Object.prototype && prototype !== null) {
      throw new EuphoError('non_canonical_value', 'Only plain JSON objects are supported');
    }
    if (Object.getOwnPropertySymbols(value).length > 0) {
      throw new EuphoError('non_canonical_value', 'Symbol-keyed properties are forbidden');
    }
    const record = value as Record<string, unknown>;
    const entries = Object.keys(record)
      .sort()
      .map((key) => {
        assertUnicodeScalarString(key);
        const entry = record[key];
        if (entry === undefined) {
          throw new EuphoError('non_canonical_value', `Undefined value at key ${key}`);
        }
        return `${JSON.stringify(key)}:${serialize(entry, ancestors)}`;
      });
    return `{${entries.join(',')}}`;
  } finally {
    ancestors.delete(value);
  }
}

function assertUnicodeScalarString(value: string): void {
  for (let index = 0; index < value.length; index += 1) {
    const unit = value.charCodeAt(index);
    if (unit >= 0xd800 && unit <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) {
        throw new EuphoError('non_canonical_value', 'Lone Unicode surrogate is forbidden');
      }
      index += 1;
    } else if (unit >= 0xdc00 && unit <= 0xdfff) {
      throw new EuphoError('non_canonical_value', 'Lone Unicode surrogate is forbidden');
    }
  }
}
