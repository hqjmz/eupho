import assert from 'node:assert/strict';
import test from 'node:test';

import { terminalText } from '../src/cli/terminal-text.js';

test('untrusted GitHub titles cannot inject terminal commands or extra lines', () => {
  const hostile = 'Fix docs\n\u001b]0;forged title\u0007\u001b[31mred\u001b[0m\u202eabc';
  const rendered = terminalText(hostile);

  assert.equal(rendered.includes('\n'), false);
  assert.equal(rendered.includes('\u001b'), false);
  assert.equal(rendered.includes('\u0007'), false);
  assert.equal(rendered.includes('\u202e'), false);
  assert.match(rendered, /^Fix docs /);
});

test('terminal text is bounded', () => {
  const rendered = terminalText('x'.repeat(500), 20);
  assert.equal(rendered.length, 20);
  assert.match(rendered, /…$/u);
});
