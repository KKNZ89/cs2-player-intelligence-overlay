import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { connectDevTools, fetchDevTools } from '../scripts/screenshot-runtime.mjs';

class Socket extends EventTarget {
  readyState = 1;
  sent = [];
  send(data) { this.sent.push(JSON.parse(data)); }
  message(data) { this.dispatchEvent(new MessageEvent('message', { data: JSON.stringify(data) })); }
  close() { this.readyState = 3; this.dispatchEvent(new Event('close')); }
}

test('DevTools requests resolve results and reject protocol errors', async () => {
  const socket = new Socket();
  const tab = await connectDevTools(socket);
  try {
    const result = tab.send('Runtime.enable');
    socket.message({ id: socket.sent[0].id, result: { enabled: true } });
    assert.deepEqual(await result, { enabled: true });
    const rejected = assert.rejects(tab.send('Bad.command'), /Unknown command/);
    socket.message({ id: socket.sent[1].id, error: { message: 'Unknown command' } });
    await rejected;
  } finally { tab.close(); }
});

test('an unanswered command times out and late replies do not block later commands', { timeout: 1000 }, async () => {
  const socket = new Socket();
  const tab = await connectDevTools(socket, { timeoutMs: 15 });
  try {
    await assert.rejects(tab.send('Page.captureScreenshot'), /Page.captureScreenshot timed out/);
    socket.message({ id: socket.sent[0].id, result: { late: true } });
    const next = tab.send('Runtime.enable');
    socket.message({ id: socket.sent[1].id, result: {} });
    assert.deepEqual(await next, {});
  } finally { tab.close(); }
});

test('disconnect rejects every pending command and future commands', async () => {
  const socket = new Socket();
  const tab = await connectDevTools(socket);
  const first = assert.rejects(tab.send('Runtime.enable'), /WebSocket closed/);
  const second = assert.rejects(tab.send('Page.enable'), /WebSocket closed/);
  socket.close();
  await Promise.all([first, second]);
  await assert.rejects(tab.send('Page.navigate'), /WebSocket closed/);
});

test('WebSocket errors reject pending commands', async () => {
  const socket = new Socket();
  const tab = await connectDevTools(socket);
  try {
    const rejected = assert.rejects(tab.send('Runtime.enable'), /WebSocket error/);
    socket.dispatchEvent(new Event('error'));
    await rejected;
  } finally { tab.close(); }
});

test('connection timeout closes an unopened socket', { timeout: 1000 }, async () => {
  const socket = new Socket();
  socket.readyState = 0;
  await assert.rejects(connectDevTools(socket, { timeoutMs: 15 }), /connection timed out/);
  assert.equal(socket.readyState, 3);
});

test('event subscriptions observe multiple errors and can be removed', async () => {
  const socket = new Socket();
  const tab = await connectDevTools(socket);
  try {
    const events = [];
    const stop = tab.on('Runtime.exceptionThrown', params => events.push(params));
    socket.message({ method: 'Runtime.exceptionThrown', params: { text: 'first' } });
    socket.message({ method: 'Runtime.exceptionThrown', params: { text: 'second' } });
    stop();
    socket.message({ method: 'Runtime.exceptionThrown', params: { text: 'third' } });
    assert.deepEqual(events, [{ text: 'first' }, { text: 'second' }]);
  } finally { tab.close(); }
});

test('malformed DevTools messages reject pending work instead of crashing', async () => {
  const socket = new Socket();
  const tab = await connectDevTools(socket);
  const rejected = assert.rejects(tab.send('Runtime.enable'), /Invalid DevTools message/);
  socket.dispatchEvent(new MessageEvent('message', { data: '{' }));
  await rejected;
  assert.equal(socket.readyState, 3);
});

test('DevTools HTTP requests reject failed responses and stalled servers', { timeout: 2000 }, async t => {
  const server = http.createServer((request, response) => {
    if (request.url === '/error') response.writeHead(500).end('failed');
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(async () => {
    const closed = new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
    server.closeAllConnections();
    await closed;
  });
  const base = `http://127.0.0.1:${server.address().port}`;
  await assert.rejects(fetchDevTools(`${base}/error`), /HTTP 500/);
  await assert.rejects(fetchDevTools(`${base}/stall`, {}, 15), error => error.name === 'TimeoutError');
});
