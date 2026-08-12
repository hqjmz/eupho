import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { resolve } from 'node:path';
import test from 'node:test';
import { promisify } from 'node:util';

const execFileAsync = promisify(execFile);
const executable = resolve('dist/src/cli.js');

test('help identifies the executable and the observe-only boundary', async () => {
  const result = await execFileAsync(process.execPath, [executable, 'help'], { encoding: 'utf8' });
  assert.match(result.stdout, /Eupho 0\.1\.0/);
  assert.match(result.stdout, /Phase 1 is observe-only/);
  assert.doesNotMatch(result.stdout, /apply|claim --/i);
});

test('once requires an explicit repository before any GitHub access', async () => {
  await assert.rejects(
    execFileAsync(process.execPath, [executable, 'once'], { encoding: 'utf8' }),
    (error: unknown) => {
      assert.equal(typeof error, 'object');
      assert.match(String((error as { stderr?: unknown }).stderr), /--repo is required/);
      assert.equal((error as { code?: unknown }).code, 2);
      return true;
    },
  );
});

test('unknown command options fail rather than being ignored', async () => {
  await assert.rejects(
    execFileAsync(process.execPath, [executable, 'status', '--mutate'], { encoding: 'utf8' }),
    (error: unknown) => {
      assert.match(String((error as { stderr?: unknown }).stderr), /--mutate requires a value/);
      assert.equal((error as { code?: unknown }).code, 2);
      return true;
    },
  );
});
