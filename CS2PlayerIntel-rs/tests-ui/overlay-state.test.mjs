import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { matchState } from '../scripts/fixtures/match-state.mjs';
import { connectDevTools, evaluate, fetchDevTools, stopBrowser, waitFor, waitForBrowser, withCleanup } from '../scripts/screenshot-runtime.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const edge = ['C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe', 'C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe'].find(existsSync);
const full = matchState({ overlayView: 'interactive', menuOpen: true });
const initial = { ...full, players: full.players.slice(0, 5) };
const pageSnapshot = readFileSync(path.join(root, 'src-tauri', 'src', 'providers', 'page-snapshot.js'), 'utf8');
const denyCookies = readFileSync(path.join(root, 'src-tauri', 'src', 'providers', 'csrep.rs'), 'utf8').match(/const DENY_COOKIES: &str = r#"([\s\S]*?)"#;/)?.[1];
if (!denyCookies) throw new Error('CSRep cookie refusal script is missing');

test('overlay roster updates survive interactive pointer gestures', { skip: edge ? false : 'Microsoft Edge is required for overlay browser regressions' }, async t => {
  const profile = mkdtempSync(path.join(tmpdir(), 'cs2intel-overlay-test-'));
  let child;
  let tab;
  let port;
  const errors = [];
  await withCleanup(async () => {
    child = spawn(edge, ['--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check', '--allow-file-access-from-files', '--remote-debugging-port=0', `--user-data-dir=${profile}`, 'about:blank'], { stdio: ['ignore', 'ignore', 'pipe'] });
    let stderr = '';
    child.stderr.on('data', chunk => {
      stderr += chunk;
      const match = stderr.match(/DevTools listening on ws:\/\/127\.0\.0\.1:(\d+)/);
      if (match) port = Number(match[1]);
    });
    await waitForBrowser(child, () => port);
    const targets = await (await fetchDevTools(`http://127.0.0.1:${port}/json/list`)).json();
    tab = await connectDevTools(new WebSocket(targets.find(target => target.type === 'page').webSocketDebuggerUrl));
    tab.on('Runtime.exceptionThrown', params => errors.push(params.exceptionDetails));
    await tab.send('Page.enable');
    await tab.send('Runtime.enable');
    await tab.send('Emulation.setDeviceMetricsOverride', { width: 1280, height: 1000, deviceScaleFactor: 1, mobile: false });
    await tab.send('Page.addScriptToEvaluateOnNewDocument', { source: `
      window.testState = ${JSON.stringify(initial)};
      window.invokeCalls = [];
      window.pushState = state => { window.testState = state; window.stateListener({ payload: state }); };
      window.__TAURI__ = {
        core: { invoke: (command, args) => {
          window.invokeCalls.push({ command, args });
          return Promise.resolve(command === 'state_get' ? window.testState : true);
        } },
        event: { listen: (name, callback) => {
          if (name === 'state:update') window.stateListener = callback;
          return Promise.resolve(() => {});
        } }
      };` });
    const push = state => evaluate(tab, `window.pushState(${JSON.stringify(state)})`);
    const rows = () => evaluate(tab, "document.querySelectorAll('#overlayPlayers [data-player]').length");
    const waitCount = count => waitFor(tab, `document.querySelectorAll('#overlayPlayers [data-player]').length === ${count} && document.getElementById('overlayCount').textContent.startsWith('${count}/10')`, `${count}-player overlay`);
    const reset = async () => {
      await tab.send('Page.navigate', { url: pathToFileURL(path.join(root, 'ui', 'overlay.html')).href });
      await waitCount(5);
    };
    const down = async (button = 'left') => {
      const point = await evaluate(tab, `(() => {
        const row = document.querySelector('#overlayPlayers [data-player]');
        window.originalRow = row;
        row.addEventListener('pointerdown', event => {
          window.actualPointerId = event.pointerId;
          window.captureTarget = event.target;
        }, { once: true });
        const rect = row.getBoundingClientRect();
        return { x: rect.x + 100, y: rect.y + rect.height / 2 };
      })()`);
      await tab.send('Input.dispatchMouseEvent', { type: 'mousePressed', ...point, button, clickCount: 1 });
      return point;
    };
    const up = point => tab.send('Input.dispatchMouseEvent', { type: 'mouseReleased', ...point, button: 'left', clickCount: 1 });
    const clickControl = async selector => {
      const point = await evaluate(tab, `(() => {
        const control = document.querySelector(${JSON.stringify(selector)});
        if (!control || control.disabled) throw new Error('Missing or disabled overlay control');
        const rect = control.getBoundingClientRect();
        return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 };
      })()`);
      await tab.send('Input.dispatchMouseEvent', { type: 'mousePressed', ...point, button: 'left', clickCount: 1 });
      await up(point);
    };

    await t.test('ordinary updates grow five players to ten', async () => {
      await reset();
      await push(full);
      await waitCount(10);
    });
    await t.test('queued updates preserve the clicked row and latest roster', async () => {
      await reset();
      const point = await down();
      await push(full);
      assert.equal(await rows(), 5);
      assert.equal(await evaluate(tab, 'window.originalRow.isConnected'), true);
      await up(point);
      await waitCount(10);
      assert.equal(await evaluate(tab, "document.querySelectorAll('.ov-detail-row').length"), 1);
    });
    await t.test('leaving the Esc view recovers without pointerup', async () => {
      await reset();
      const point = await down();
      await push(full);
      assert.equal(await rows(), 5);
      await push({ ...full, overlayView: 'compact', setup: { ...full.setup, menuOpen: false } });
      await waitCount(10);
      await up(point);
      await push({ ...initial, overlayView: 'compact' });
      await waitCount(5);
      await push({ ...full, overlayView: 'compact' });
      await waitCount(10);
    });
    await t.test('capture loss releases a queued roster', async () => {
      await reset();
      const point = await down();
      await push(full);
      await tab.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: point.x + 10, y: point.y, button: 'left', buttons: 1 });
      assert.equal(await evaluate(tab, 'window.captureTarget.hasPointerCapture(window.actualPointerId)'), true);
      await evaluate(tab, 'window.captureTarget.releasePointerCapture(window.actualPointerId)');
      await tab.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: point.x + 20, y: point.y, button: 'left', buttons: 1 });
      await waitCount(10);
      await up(point);
    });
    await t.test('pointer cancellation releases only the active gesture', async () => {
      await reset();
      const point = await down();
      await push(full);
      await evaluate(tab, "window.dispatchEvent(new PointerEvent('pointercancel', { pointerId: window.actualPointerId + 100 }))");
      assert.equal(await rows(), 5);
      await evaluate(tab, "window.dispatchEvent(new PointerEvent('pointercancel', { pointerId: window.actualPointerId }))");
      await waitCount(10);
      await up(point);
    });
    await t.test('release outside the roster is captured and refreshes it', async () => {
      await reset();
      await down();
      await push(full);
      await tab.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: 1250, y: 900, button: 'left', buttons: 1 });
      await up({ x: 1250, y: 900 });
      await waitCount(10);
    });
    await t.test('right clicks do not stall incoming roster updates', async () => {
      await reset();
      const point = await down('right');
      await push(full);
      await waitCount(10);
      await tab.send('Input.dispatchMouseEvent', { type: 'mouseReleased', ...point, button: 'right', clickCount: 1 });
    });
    await t.test('interactive shortcut hints require Esc while CS2 is playing', async () => {
      await reset();
      await push({ ...initial, gsi: { ...initial.gsi, activity: 'playing' } });
      assert.match(await evaluate(tab, "document.getElementById('ovHint').textContent"), /Press Esc in CS2 to release the mouse/);
      await push({ ...initial, gsi: { ...initial.gsi, activity: 'menu' } });
      assert.match(await evaluate(tab, "document.getElementById('ovHint').textContent"), /^Esc menu: click a player/);
    });
    await t.test('mouse clicks on every profile link reach the bridge with the selected player', async () => {
      await reset();
      await up(await down());
      await waitFor(tab, "document.querySelectorAll('.ov-detail-row').length === 1", 'player card');
      const providers = ['leetify', 'csrep', 'csstats', 'steam'];
      for (const provider of providers) {
        await clickControl(`[data-action="profile"][data-provider="${provider}"]`);
        await waitFor(tab, `window.invokeCalls.some(call => call.command === 'profile_open' && call.args.provider === '${provider}')`, `${provider} profile command`);
      }
      assert.deepEqual(await evaluate(tab, "window.invokeCalls.filter(call => call.command === 'profile_open').map(call => call.args)"),
        providers.map(provider => ({ provider, steamId: initial.players[0].steamId })));
    });
    await t.test('unknown team headers explain how to assign a side in both overlay layouts', async () => {
      await reset();
      assert.match(await evaluate(tab, "document.querySelector('.ov-group.unknown td').title"), /Steam recent players do not identify teams/);
      await push({ ...initial, overlayView: 'compact', settings: { ...initial.settings, overlayLayout: 'right' } });
      assert.match(await evaluate(tab, "document.querySelector('.ov-team.unknown .ov-team-name').title"), /Team\/Enemy/);
    });
    await t.test('explicit mouse navigation offers an Esc exit even while GSI reports playing', async () => {
      await reset();
      await push({ ...full, overlayMouseNavigation: true, gsi: { ...full.gsi, activity: 'playing' } });
      await waitCount(10);
      assert.match(await evaluate(tab, "document.getElementById('ovHint').textContent"), /^Mouse navigation:/);
      await tab.send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27 });
      await tab.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27 });
      await waitFor(tab, "window.invokeCalls.some(call => call.command === 'overlay_interaction_end')", 'mouse navigation exit command');
      await push({ ...full, overlayMouseNavigation: false, overlayView: 'compact' });
      assert.equal(await evaluate(tab, "document.body.classList.contains('view-compact')"), true);
    });
    await t.test('automatic Esc-menu interaction does not intercept Escape', async () => {
      await reset();
      await tab.send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27 });
      await tab.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Escape', code: 'Escape', windowsVirtualKeyCode: 27 });
      assert.equal(await evaluate(tab, "window.invokeCalls.some(call => call.command === 'overlay_interaction_end')"), false);
    });
    await t.test('website consent is detected without accepting or rejecting it', async () => {
      await tab.send('Page.navigate', { url: 'about:blank' });
      await waitFor(tab, "document.readyState === 'complete'", 'consent fixture');
      await evaluate(tab, `(() => {
        window.consentClicks = 0;
        document.body.innerHTML = '<h1>Profile</h1><p>TRUST SCORE 87%</p><div role="dialog"><p>Choose cookie preferences</p><button id="reject" onclick="window.consentClicks++; this.parentElement.remove()">Reject all</button><button onclick="window.consentClicks++">Accept all</button></div>';
      })()`);
      const before = await evaluate(tab, pageSnapshot);
      assert.equal(before.consent_required, true);
      assert.equal(await evaluate(tab, 'window.consentClicks'), 0, 'snapshot must never choose consent');
      await evaluate(tab, "document.getElementById('reject').click()");
      assert.equal((await evaluate(tab, pageSnapshot)).consent_required, false);
      assert.equal(await evaluate(tab, 'window.consentClicks'), 1, 'only the manual action chooses consent');
    });
    await t.test('hidden prompts and cookie-policy footers do not block profiles', async () => {
      await evaluate(tab, `document.body.innerHTML = '<h1>Profile</h1><footer>Cookie policy <a href="#cookies">Read more</a></footer><div role="dialog" style="display:none"><p>Cookie consent</p><button>Accept all</button></div>'`);
      assert.equal((await evaluate(tab, pageSnapshot)).consent_required, false);
    });
    await t.test('offscreen cookie banners do not count as visible prompts', async () => {
      await evaluate(tab, `document.body.innerHTML = '<h1>Profile</h1><div id="cookie-banner" style="position:fixed;top:-10000px"><p>Cookie preferences</p><button>Accept all</button></div>'`);
      assert.equal((await evaluate(tab, pageSnapshot)).consent_required, false);
    });
    await t.test('named consent banners require a manual choice and clear after it', async () => {
      await evaluate(tab, `document.body.innerHTML = '<h1>Profile</h1><div id="cookie-banner"><p>We use cookies</p><input type="button" value="Manage preferences"></div>'`);
      assert.equal((await evaluate(tab, pageSnapshot)).consent_required, true);
      await evaluate(tab, "document.getElementById('cookie-banner').remove()");
      assert.equal((await evaluate(tab, pageSnapshot)).consent_required, false);
    });
    await t.test('CSRep retains its existing Deny all choice without accepting optional cookies', async () => {
      await evaluate(tab, `(() => {
        window.consentClicks = 0;
        document.body.innerHTML = '<div role="dialog"><p>Cookie preferences</p><button onclick="window.consentClicks++; this.parentElement.remove()">Deny all</button><button onclick="throw new Error(\\'Optional cookies must not be accepted automatically\\')">Accept all</button></div>';
      })()`);
      assert.equal((await evaluate(tab, pageSnapshot)).consent_required, true);
      assert.equal(await evaluate(tab, denyCookies), true);
      assert.equal(await evaluate(tab, 'window.consentClicks'), 1);
      assert.equal((await evaluate(tab, pageSnapshot)).consent_required, false);
    });
    await t.test('CSRep leaves consent for the user when Deny all is unavailable', async () => {
      await evaluate(tab, `(() => {
        window.consentClicks = 0;
        document.body.innerHTML = '<div role="dialog"><p>Cookie consent</p><button onclick="window.consentClicks++">Accept all</button></div>';
      })()`);
      assert.equal(await evaluate(tab, denyCookies), false);
      assert.equal(await evaluate(tab, 'window.consentClicks'), 0);
      assert.equal((await evaluate(tab, pageSnapshot)).consent_required, true);
    });
    assert.deepEqual(errors, [], 'overlay must not raise browser exceptions');
  }, async () => {
    tab?.close();
    try { if (child) await stopBrowser(child); }
    finally { rmSync(profile, { recursive: true, force: true }); }
  });
});
