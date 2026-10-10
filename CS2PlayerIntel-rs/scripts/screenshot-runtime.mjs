import { ChildProcess, spawnSync } from 'node:child_process';
import { setTimeout as sleep } from 'node:timers/promises';

export const TIMEOUT_MS = 10_000;

export async function fetchDevTools(url, options = {}, timeoutMs = TIMEOUT_MS) {
  const response = await fetch(url, { ...options, signal: AbortSignal.timeout(timeoutMs) });
  if (!response.ok) throw new Error(`DevTools HTTP ${response.status}: ${url}`);
  return response;
}

export async function connectDevTools(socket, { timeoutMs = TIMEOUT_MS } = {}) {
  let id = 0;
  let failure = null;
  const pending = new Map();
  const listeners = new Map();
  let resolveReady;
  let rejectReady;
  const ready = new Promise((resolve, reject) => { resolveReady = resolve; rejectReady = reject; });
  const readyTimer = setTimeout(() => fail(new Error('DevTools connection timed out')), timeoutMs);

  function fail(error) {
    if (failure) return;
    failure = error;
    clearTimeout(readyTimer);
    rejectReady(error);
    for (const request of [...pending.values()]) request.reject(error);
  }

  socket.addEventListener('open', () => { clearTimeout(readyTimer); resolveReady(); });
  socket.addEventListener('error', () => fail(new Error('DevTools WebSocket error')));
  socket.addEventListener('close', () => fail(new Error('DevTools WebSocket closed')));
  socket.addEventListener('message', event => {
    let message;
    try { message = JSON.parse(event.data); }
    catch (error) { fail(new Error('Invalid DevTools message', { cause: error })); socket.close(); return; }
    const request = pending.get(message.id);
    if (request) {
      if (message.error) request.reject(new Error(message.error.message));
      else request.resolve(message.result);
    } else if (message.method) {
      for (const listener of listeners.get(message.method) || []) listener(message.params);
    }
  });
  if (socket.readyState === 1) { clearTimeout(readyTimer); resolveReady(); }
  else if (socket.readyState === 3) fail(new Error('DevTools WebSocket is already closed'));
  try { await ready; }
  catch (error) { socket.close(); throw error; }

  return {
    send(method, params = {}) {
      if (failure) return Promise.reject(failure);
      return new Promise((resolve, reject) => {
        const requestId = ++id;
        const finish = callback => value => {
          clearTimeout(timer);
          pending.delete(requestId);
          callback(value);
        };
        const timer = setTimeout(() => request.reject(new Error(`DevTools ${method} timed out`)), timeoutMs);
        const request = { resolve: finish(resolve), reject: finish(reject) };
        pending.set(requestId, request);
        try { socket.send(JSON.stringify({ id: requestId, method, params })); }
        catch (error) { request.reject(error); }
      });
    },
    on(method, listener) {
      if (!listeners.has(method)) listeners.set(method, new Set());
      const group = listeners.get(method);
      group.add(listener);
      return () => group.delete(listener);
    },
    close() {
      fail(new Error('DevTools connection closed by capture'));
      listeners.clear();
      socket.close();
    }
  };
}

export async function evaluate(tab, expression) {
  const response = await tab.send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
  if (response.exceptionDetails) {
    const details = response.exceptionDetails;
    throw new Error(`Page evaluation failed: ${details.exception?.description || details.text || 'Unknown exception'}`);
  }
  return response.result?.value;
}

export async function waitFor(tab, expression, label, { timeoutMs = TIMEOUT_MS, pollMs = 50 } = {}) {
  const deadline = Date.now() + timeoutMs;
  do {
    if (await evaluate(tab, expression) === true) return;
    if (Date.now() >= deadline) break;
    await sleep(Math.min(pollMs, Math.max(0, deadline - Date.now())));
  } while (Date.now() <= deadline);
  throw new Error(`Timed out waiting for ${label}`);
}

export async function click(tab, selector) {
  const encoded = JSON.stringify(selector);
  await waitFor(tab, `(() => {
    const element = document.querySelector(${encoded});
    return Boolean(element && !element.disabled && element.getClientRects().length);
  })()`, `clickable ${selector}`);
  await evaluate(tab, `(() => {
    const element = document.querySelector(${encoded});
    if (!element) throw new Error('Missing screenshot control: ' + ${encoded});
    element.click();
  })()`);
}

export async function waitForBrowser(child, readPort, { timeoutMs = TIMEOUT_MS, pollMs = 50 } = {}) {
  let failure = null;
  const onError = error => { failure = new Error(`Headless Edge could not start: ${error.message}`, { cause: error }); };
  const onExit = (code, signal) => { failure = new Error(`Headless Edge exited before readiness (${signal || code})`); };
  child.on('error', onError);
  child.on('exit', onExit);
  const deadline = Date.now() + timeoutMs;
  try {
    do {
      if (failure) throw failure;
      if (child.exitCode !== null || child.signalCode !== null) throw new Error('Headless Edge exited before readiness');
      let port;
      try { port = readPort(); }
      catch (error) { if (error.code !== 'ENOENT') throw error; }
      if (port) {
        if (!/^\d+$/.test(port) || Number(port) < 1 || Number(port) > 65535) throw new Error('Invalid Edge debugging port');
        return port;
      }
      if (Date.now() >= deadline) break;
      await sleep(Math.min(pollMs, Math.max(0, deadline - Date.now())));
    } while (Date.now() <= deadline);
    throw new Error('Headless Edge did not start before the timeout');
  } finally {
    child.off('error', onError);
    child.off('exit', onExit);
  }
}

// On Windows, kill() ends only Edge's main process; its helper processes keep the output pipe open, so
// 'close' waits until they notice, which can take long on a busy machine. Ending the process tree is immediate.
function kill(child) {
  if (process.platform === 'win32' && child instanceof ChildProcess) {
    if (spawnSync('taskkill', ['/PID', String(child.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true }).status === 0) return true;
  }
  return child.kill();
}

export async function stopBrowser(child, timeoutMs = TIMEOUT_MS) {
  if (!child?.pid || child.exitCode !== null || child.signalCode !== null) return;
  await new Promise((resolve, reject) => {
    const finish = error => {
      clearTimeout(timer);
      child.off('close', onClose);
      child.off('error', onError);
      if (error) reject(error); else resolve();
    };
    const onClose = () => finish();
    const onError = error => finish(error);
    const timer = setTimeout(() => finish(new Error('Headless Edge did not exit before the timeout')), timeoutMs);
    child.once('close', onClose);
    child.once('error', onError);
    try {
      if (!kill(child)) finish(new Error('Could not stop Headless Edge'));
    } catch (error) { finish(error); }
  });
}

export async function withCleanup(work, cleanup) {
  let failure;
  try { return await work(); }
  catch (error) { failure = error; throw error; }
  finally {
    try { await cleanup(); }
    catch (error) {
      if (failure) throw new AggregateError([failure, error], `${failure.message}; cleanup failed: ${error.message}`);
      throw error;
    }
  }
}
