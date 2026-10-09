import test from 'node:test';
import assert from 'node:assert/strict';
import vm from 'node:vm';
import { click, evaluate, waitFor } from '../scripts/screenshot-runtime.mjs';

function page(context) {
  const sandbox = vm.createContext(context);
  return {
    async send(method, params) {
      assert.equal(method, 'Runtime.evaluate');
      assert.equal(params.awaitPromise, true);
      assert.equal(params.returnByValue, true);
      try { return { result: { value: await vm.runInContext(params.expression, sandbox) } }; }
      catch (error) { return { exceptionDetails: { exception: { description: error.message } } }; }
    }
  };
}

test('page evaluation rejects synchronous and asynchronous exceptions', async () => {
  const tab = page({});
  await assert.rejects(evaluate(tab, "throw new Error('setup failed')"), /setup failed/);
  await assert.rejects(evaluate(tab, "Promise.reject(new Error('async setup failed'))"), /async setup failed/);
  assert.equal(await evaluate(tab, 'Promise.resolve(42)'), 42);
});

test('page evaluation rejects exceptionDetails even without a Runtime exception event', async () => {
  const tab = { send: async () => ({ exceptionDetails: { text: 'Uncaught setup exception' } }) };
  await assert.rejects(evaluate(tab, 'setup()'), /Uncaught setup exception/);
});

test('readiness waits for the requested state rather than any truthy response', async () => {
  let checks = 0;
  const tab = { send: async () => ({ result: { value: ++checks === 1 ? 'not ready' : checks >= 3 } }) };
  await waitFor(tab, 'ready', 'rendered history', { pollMs: 1 });
  assert.equal(checks, 3);
});

test('readiness fails with a named timeout when the intended view never appears', { timeout: 1000 }, async () => {
  await assert.rejects(waitFor(page({}), 'false', 'selected Matches tab', { timeoutMs: 15, pollMs: 1 }), /Timed out waiting for selected Matches tab/);
});

test('readiness evaluation errors are not retried as missing state', async () => {
  let checks = 0;
  const tab = { send: async () => { checks++; return { exceptionDetails: { text: 'Bad selector' } }; } };
  await assert.rejects(waitFor(tab, 'ready', 'view'), /Bad selector/);
  assert.equal(checks, 1);
});

test('click waits for a visible enabled control and verifies its resulting state', async () => {
  const state = { visible: false, disabled: true, selected: false };
  let lookups = 0;
  const element = {
    get disabled() { return state.disabled; },
    getClientRects: () => state.visible ? [{}] : [],
    click: () => { state.selected = true; }
  };
  const tab = page({
    document: {
      querySelector: selector => {
        assert.equal(selector, '[data-tab="matches"]');
        if (++lookups > 1) { state.visible = true; state.disabled = false; }
        return element;
      }
    },
    state
  });
  await click(tab, '[data-tab="matches"]');
  await waitFor(tab, 'state.selected', 'selected Matches tab');
  assert.equal(state.selected, true);
  assert.ok(lookups >= 3);
});

test('a control disappearing between readiness and click fails explicitly', async () => {
  let lookups = 0;
  const tab = page({
    document: {
      querySelector: () => ++lookups === 1 ? { disabled: false, getClientRects: () => [{}] } : null
    }
  });
  await assert.rejects(click(tab, '#missing'), /Missing screenshot control: #missing/);
});
