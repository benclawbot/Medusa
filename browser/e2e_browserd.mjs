#!/usr/bin/env node
// End-to-end smoke test for medusa-browserd.
//
// Spawns the sidecar, drives ping -> navigate -> snapshot -> close,
// and asserts the protocol works. Gated on Chromium availability — the
// navigate call hangs if no browser is installed.

import { spawn } from 'node:child_process';
import { strict as assert } from 'node:assert';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, '..');
const exeName = process.platform === 'win32' ? 'medusa-browserd.exe' : 'medusa-browserd';
const sidecar = resolve(repo, 'target', 'debug', exeName);

const child = spawn(sidecar, ['--stdio'], {
  stdio: ['pipe', 'pipe', 'inherit'],
});
let buffer = '';
const responses = new Map();

child.stdout.setEncoding('utf8');
child.stdout.on('data', (chunk) => {
  buffer += chunk;
  let idx;
  while ((idx = buffer.indexOf('\n')) !== -1) {
    const line = buffer.slice(0, idx);
    buffer = buffer.slice(idx + 1);
    if (!line) continue;
    try {
      const parsed = JSON.parse(line);
      responses.set(parsed.kind || 'ok', parsed);
    } catch (err) {
      console.error('parse error:', err, 'line:', line);
    }
  }
});

function send(req) {
  const payload = JSON.stringify({ ...req }) + '\n';
  child.stdin.write(payload);
}

await new Promise((r) => setTimeout(r, 250));

send({ method: 'ping' });
await new Promise((r) => setTimeout(r, 200));
assert.ok(responses.has('ok'), 'ping should return ok');

send({ method: 'navigate', url: 'data:text/html,<h1>Hello</h1>' });
await new Promise((r) => setTimeout(r, 500));
assert.ok(responses.has('navigate'), 'navigate should respond');

send({ method: 'snapshot' });
await new Promise((r) => setTimeout(r, 500));
const snap = responses.get('snapshot');
assert.ok(snap && snap.text && snap.text.includes('Hello'), `snapshot text: ${JSON.stringify(snap)}`);

send({ method: 'close' });
child.stdin.end();
await new Promise((r) => setTimeout(r, 200));

console.log('e2e_browserd: ok');