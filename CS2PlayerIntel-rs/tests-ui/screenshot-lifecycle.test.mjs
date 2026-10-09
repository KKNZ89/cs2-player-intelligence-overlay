import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { existsSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { stopBrowser, waitForBrowser, withCleanup } from '../scripts/screenshot-runtime.mjs';

function child() {
  return Object.assign(new EventEmitter(), { pid: 123, exitCode: null, signalCode: null });
}

test('browser readiness retries a missing port file and accepts a valid port', async () => {
  const process = child();
  let reads = 0;
  const port = await waitForBrowser(process, () => {
    if (++reads === 1) throw Object.assign(new Error('not written'), { code: 'ENOENT' });
    return '9222';
  }, { pollMs: 1 });
  assert.equal(port, '9222');
  assert.equal(reads, 2);
  assert.equal(process.listenerCount('error'), 0);
  assert.equal(process.listenerCount('exit'), 0);
});

test('browser startup errors propagate and still remove allocated resources', async () => {
  const process = child();
  const folder = mkdtempSync(path.join(tmpdir(), 'cs2intel-runtime-test-'));
  let cleaned = false;
  try {
    await assert.rejects(withCleanup(async () => {
      queueMicrotask(() => process.emit('error', new Error('spawn denied')));
      await waitForBrowser(process, () => '', { pollMs: 1 });
    }, () => { rmSync(folder, { recursive: true }); cleaned = true; }), /spawn denied/);
    assert.equal(cleaned, true);
    assert.equal(existsSync(folder), false);
  } finally { if (existsSync(folder)) rmSync(folder, { recursive: true }); }
});

test('browser exit and startup timeout fail within a bounded wait', { timeout: 1000 }, async () => {
  const process = child();
  queueMicrotask(() => process.emit('exit', 1, null));
  await assert.rejects(waitForBrowser(process, () => '', { pollMs: 1 }), /exited before readiness/);
  await assert.rejects(waitForBrowser(child(), () => '', { timeoutMs: 15, pollMs: 1 }), /did not start before the timeout/);
});

test('invalid ports and filesystem errors are not hidden as browser startup delays', async () => {
  await assert.rejects(waitForBrowser(child(), () => '70000'), /Invalid Edge debugging port/);
  const denied = Object.assign(new Error('access denied'), { code: 'EACCES' });
  await assert.rejects(waitForBrowser(child(), () => { throw denied; }), error => error === denied);
});

test('browser shutdown waits for process close and releases its listeners', async () => {
  const process = child();
  process.kill = () => { queueMicrotask(() => process.emit('close', 0)); return true; };
  await stopBrowser(process);
  assert.equal(process.listenerCount('close'), 0);
  assert.equal(process.listenerCount('error'), 0);
});

test('browser shutdown reports kill failures and exit timeouts', { timeout: 1000 }, async () => {
  const refused = child();
  refused.kill = () => false;
  await assert.rejects(stopBrowser(refused), /Could not stop/);
  const stuck = child();
  stuck.kill = () => true;
  await assert.rejects(stopBrowser(stuck, 15), /did not exit before the timeout/);
  assert.equal(stuck.listenerCount('close'), 0);
});

test('cleanup preserves the original failure and reports cleanup failures too', async () => {
  const original = new Error('startup failed');
  const cleanup = new Error('profile cleanup failed');
  await assert.rejects(withCleanup(() => { throw original; }, () => { throw cleanup; }), error => {
    assert.ok(error instanceof AggregateError);
    assert.deepEqual(error.errors, [original, cleanup]);
    assert.match(error.message, /startup failed; cleanup failed: profile cleanup failed/);
    return true;
  });
});

test('cleanup runs on success and also reports a cleanup-only failure', async () => {
  let cleaned = false;
  assert.equal(await withCleanup(() => 42, () => { cleaned = true; }), 42);
  assert.equal(cleaned, true);
  await assert.rejects(withCleanup(() => 42, () => { throw new Error('cleanup failed'); }), /cleanup failed/);
});
