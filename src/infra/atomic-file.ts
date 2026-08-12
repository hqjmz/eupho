import { randomUUID } from 'node:crypto';
import { chmod, mkdir, open, readFile, rename, unlink } from 'node:fs/promises';
import { basename, dirname, join } from 'node:path';

import { EuphoError, messageOf } from '../errors.js';

export async function ensurePrivateDirectory(path: string): Promise<void> {
  await mkdir(path, { recursive: true, mode: 0o700 });
  await chmod(path, 0o700);
}

export async function atomicWriteJson(path: string, value: unknown): Promise<void> {
  const parent = dirname(path);
  await ensurePrivateDirectory(parent);
  const temporary = join(parent, `.${basename(path)}.${process.pid}.${randomUUID()}.tmp`);
  let renamed = false;
  try {
    const handle = await open(temporary, 'wx', 0o600);
    try {
      await handle.writeFile(`${JSON.stringify(value, null, 2)}\n`, 'utf8');
      await handle.sync();
    } finally {
      await handle.close();
    }

    await rename(temporary, path);
    renamed = true;
    await chmod(path, 0o600);
    const directory = await open(parent, 'r');
    try {
      await directory.sync();
    } finally {
      await directory.close();
    }
  } catch (error) {
    throw new EuphoError('atomic_write_failed', `Cannot atomically write ${path}: ${messageOf(error)}`, 1, {
      cause: error,
    });
  } finally {
    if (!renamed) await unlink(temporary).catch(() => undefined);
  }
}

export async function readJsonFile<T>(path: string): Promise<T> {
  try {
    return JSON.parse(await readFile(path, 'utf8')) as T;
  } catch (error) {
    throw new EuphoError('state_read_failed', `Cannot read valid JSON from ${path}: ${messageOf(error)}`, 1, {
      cause: error,
    });
  }
}
