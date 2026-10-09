// Renders the dashboard and overlay with fictional fixture data and saves PNGs for design review:
//   node scripts/screenshots.mjs [output-folder]
// Serves the real pages on loopback with a stand-in for the Tauri bridge and captures them with headless
// Microsoft Edge (installed with Windows) over the DevTools protocol. No CS2, Steam or provider access.
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import http from 'node:http';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { matchState, offlineState } from './fixtures/match-state.mjs';
import { click, connectDevTools, evaluate, fetchDevTools, stopBrowser, waitFor, waitForBrowser, withCleanup } from './screenshot-runtime.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const ui = path.join(root, 'ui');
const { version } = JSON.parse(readFileSync(path.join(root, 'package.json'), 'utf8'));
const output = path.resolve(process.argv[2] || path.join(root, 'docs', 'screenshots'));
const EDGE = ['C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe', 'C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe'].find(existsSync);
if (!EDGE) throw new Error('Microsoft Edge was not found.');

const VARIANTS = {
  default: () => matchState(),
  top: () => matchState({ overlayLayout: 'top' }),
  expanded: () => matchState({ overlayView: 'expanded' }),
  interactive: () => matchState({ overlayView: 'interactive', menuOpen: true }),
  light: () => matchState({ theme: 'light' }),
  minimal: () => matchState({ overlayStyle: 'minimal' }),
  offline: () => offlineState()
};

// Stands in for window.__TAURI__: state comes from the fixture, every other command succeeds.
const SHIM = `(() => {
  const day = 864e5, t = Date.UTC(2026, 9, 8);
  const RECORD = { summary: { all: { played: 3, won: 1, lost: 2, tied: 0, unknown: 0 }, together: { played: 0, won: 0, lost: 0, tied: 0, unknown: 0 }, against: { played: 3, won: 1, lost: 2, tied: 0, unknown: 0 }, firstPlayedAt: t - 40 * day, lastPlayedAt: t - 2 * day },
    encounters: [{ endedAt: t - 2 * day, map: 'de_mirage', mode: 'premier', result: 'loss', side: 'enemy' }, { endedAt: t - 15 * day, map: 'de_ancient', mode: 'premier', result: 'win', side: 'enemy' }, { endedAt: t - 40 * day, map: 'de_nuke', mode: 'competitive', result: 'loss', side: 'enemy' }],
    progression: [{ provider: 'steam', metric: 'hoursCs2', first: { value: 120, measuredAt: t - 40 * day }, previous: { value: 160, measuredAt: t - 15 * day }, current: { value: 190, measuredAt: t - 2 * day }, measurements: 3 },
      { provider: 'csrep', metric: 'trust', first: { value: 97, measuredAt: t - 40 * day }, previous: null, current: { value: 99, measuredAt: t - 2 * day }, measurements: 2 }],
    notes: [{ id: 2, text: 'Strong AWP on B site. Plays with Halcyon.', createdAt: t - 2 * day, map: 'de_mirage', mode: 'premier', side: 'enemy', matchId: 42, result: 'loss' },
      { id: 1, text: 'Calls out well, friendly.', createdAt: t - 15 * day, map: 'de_ancient', mode: 'premier', side: 'team', matchId: 40, result: 'win' }] };
  const MAPS = [['de_mirage', 'premier', 'loss'], ['de_ancient', 'premier', 'win'], ['de_inferno', 'competitive', 'win'], ['de_nuke', 'premier', 'tie'], ['de_dust2', 'wingman', 'win'], ['de_anubis', 'premier', 'loss']];
  const MATCHES = { total: 42, matches: MAPS.map(([map, mode, result], i) => ({ id: 42 - i, endedAt: t - (i * 1.4 + 0.2) * day, map, mode, result, team: mode === 'wingman' ? 1 : 4, enemy: mode === 'wingman' ? 2 : 5, players: mode === 'wingman' ? 3 : 9 })) };
  const PEOPLE = [['Kestrel', 'team', 3, 3120, 96, 8, 2210, 1.21, 48], ['nova', 'team', 1, 1860, 88, 5, 1540, 0.98, 41], ['Brightside', 'team', 2, 2100, 93, 6, null, 1.10, 47], ['mxlk', 'team', 1, null, null, null, null, null, null],
    ['ZeroDay', 'enemy', 4, 190, 99, 10, 2950, 1.65, 58], ['Halcyon', 'enemy', 2, 2780, 71, 7, 1890, 1.27, 50], ['r1ft', 'enemy', 1, 1420, 90, 6, 1610, 1.01, 44], ['Paxton', 'enemy', 1, 140, 77, 2, 980, 1.33, 52], ['Lumen', 'enemy', 1, 760, 92, 5, 1450, 0.94, 39]];
  const MATCH = { id: 42, endedAt: MATCHES.matches[0].endedAt, map: 'de_mirage', mode: 'premier', result: 'loss', selfId: '76561198012345670',
    players: PEOPLE.map(([name, side, met, hours, trust, level, elo, kd, hs], i) => ({ steamId: String(76561198012345671n + BigInt(i)), name, side, met, hasNote: name === 'Halcyon',
      notes: name === 'Halcyon' ? [{ id: 3, text: 'Rushes B with the AWP every pistol round.', createdAt: MATCHES.matches[0].endedAt - 900000, map: 'de_mirage', mode: 'premier', side: 'enemy', matchId: 42, result: 'loss' }] : [],
      values: Object.fromEntries([['steam.hoursCs2', hours], ['csrep.trust', trust], ['faceit.level', level], ['faceit.elo', elo], ['csstats.kd', kd], ['csstats.hs', hs]].filter(([, v]) => v !== null)) })) };
  const params = new URLSearchParams(location.search);
  const state = fetch('/__state?variant=' + (params.get('variant') || 'default')).then(r => r.json());
  window.__TAURI__ = {
    core: { invoke: command => command === 'state_get' ? state : command === 'diagnostics_recent' ? Promise.resolve([]) : command === 'player_history' ? Promise.resolve(RECORD) : command === 'history_matches' ? Promise.resolve(MATCHES) : command === 'history_match' ? Promise.resolve(MATCH) : command === 'history_stats' ? Promise.resolve({ matches: 42, players: 311, snapshots: 2876, notes: 3, file: 'C:/Users/you/AppData/Roaming/nz.local.cs2playerintel/history.sqlite', bytes: 851968, error: '' }) : Promise.resolve(true) },
    event: { listen: () => Promise.resolve(() => {}) }
  };
})();`;

const TYPES = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.woff2': 'font/woff2', '.png': 'image/png', '.svg': 'image/svg+xml', '.json': 'application/json' };

const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://127.0.0.1');
  if (url.pathname === '/__shim.js') return res.writeHead(200, { 'content-type': 'text/javascript' }).end(SHIM);
  if (url.pathname === '/__state') {
    const state = (VARIANTS[url.searchParams.get('variant')] || VARIANTS.default)();
    return res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ ...state, version }));
  }
  const file = path.join(ui, decodeURIComponent(url.pathname));
  if (!file.startsWith(ui) || !existsSync(file)) return res.writeHead(404).end();
  let body = readFileSync(file);
  // The shim must exist before the page's module scripts run.
  if (file.endsWith('.html')) body = Buffer.from(body.toString('utf8').replace('<head>', '<head><script src="/__shim.js"></script>'));
  res.writeHead(200, { 'content-type': TYPES[path.extname(file)] || 'application/octet-stream' }).end(body);
});
let base;
let profile;
let edge;
let port;

async function open() {
  const target = await (await fetchDevTools(`http://127.0.0.1:${port}/json/new?about:blank`, { method: 'PUT' })).json();
  const closeTarget = () => fetchDevTools(`http://127.0.0.1:${port}/json/close/${target.id}`);
  let socket;
  let connection;
  await withCleanup(async () => {
    socket = new WebSocket(target.webSocketDebuggerUrl);
    connection = await connectDevTools(socket);
  }, async () => {
    if (!connection) { socket?.close(); await closeTarget(); }
  });
  return { ...connection, close: async () => { connection.close(); await closeTarget(); } };
}

async function capture(name, page, { width, height, variant = 'default', steps = [], transparent = false, fullPage = false }) {
  const tab = await open();
  const errors = [];
  const unlisten = tab.on('Runtime.exceptionThrown', params => errors.push(params.exceptionDetails?.exception?.description || params.exceptionDetails?.text || 'Unknown page exception'));
  const shot = await withCleanup(async () => {
    await tab.send('Runtime.enable');
    await tab.send('Page.enable');
    await tab.send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false });
    if (transparent) await tab.send('Emulation.setDefaultBackgroundColorOverride', { color: { r: 0, g: 0, b: 0, a: 0 } });
    const url = `${base}/${page}?${new URLSearchParams({ variant })}`;
    const navigation = await tab.send('Page.navigate', { url });
    if (navigation.errorText) throw new Error(`${name}: navigation failed: ${navigation.errorText}`);
    await waitFor(tab, `location.href === ${JSON.stringify(url)} && document.readyState === 'complete'`, `${name} page load`);
    const rendered = page === 'overlay.html'
      ? "Boolean(document.querySelector('#overlayPlayers [data-player]') && document.getElementById('overlayCount').textContent.includes('10/10'))"
      : variant === 'offline'
        ? "Boolean(document.getElementById('setupGuide') && !document.getElementById('setupGuide').hidden && document.getElementById('setupGuide').children.length)"
        : "document.querySelectorAll('#playersBody tbody tr[data-player]').length === 10";
    await waitFor(tab, rendered, `${name} fixture render`);
    for (const step of steps) {
      await click(tab, step.selector);
      await waitFor(tab, step.ready, `${name}: ${step.selector}`);
    }
    await evaluate(tab, `(async () => {
      await document.fonts.ready;
      await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    })()`);
    await waitFor(tab, `Array.from(document.images).every(image => image.complete && image.naturalWidth > 0)
      && document.getAnimations().every(animation => animation.playState !== 'running')`, `${name} visual readiness`);
    // Overlay pages size their window to the card; crop to it so the PNG shows what the window would.
    const cardHeight = page === 'overlay.html'
      ? await evaluate(tab, "Math.ceil(document.querySelector('.overlay-card').getBoundingClientRect().height)")
      : 0;
    if (page === 'overlay.html' && (!Number.isFinite(cardHeight) || cardHeight <= 0)) throw new Error(`${name}: invalid overlay height`);
    const pageHeight = fullPage
      ? await evaluate(tab, 'Math.ceil(document.documentElement.scrollHeight)')
      : height;
    if (!Number.isFinite(pageHeight) || pageHeight <= 0) throw new Error(`${name}: invalid page height`);
    const clip = { x: 0, y: 0, width, height: cardHeight ? Math.min(height, cardHeight) : pageHeight, scale: 1 };
    if (errors.length) throw new Error(`${name}: ${errors.join('; ')}`);
    const result = await tab.send('Page.captureScreenshot', { format: 'png', clip, captureBeyondViewport: fullPage });
    if (errors.length) throw new Error(`${name}: ${errors.join('; ')}`);
    return result;
  }, async () => {
    unlisten();
    await tab.close();
  });
  writeFileSync(path.join(output, `${name}.png`), Buffer.from(shot.data, 'base64'));
  console.log(`saved ${name}.png`);
}

const details = {
  selector: '[data-action="details"][data-id="76561198012345675"]',
  ready: "Boolean(document.getElementById('details') && !document.getElementById('details').hidden && document.getElementById('detailsBody').children.length)"
};
const view = name => ({
  selector: `[data-view="${name}"]`,
  ready: `Boolean(document.getElementById('view-${name}') && !document.getElementById('view-${name}').hidden && document.querySelector('[data-view="${name}"].active'))`
});
const section = name => ({
  selector: `.settings-tab[data-section="${name}"]`,
  ready: `Boolean(document.querySelector('.settings-tab[data-section="${name}"][aria-selected="true"]') && !document.querySelector('.settings-section[data-section="${name}"]').hidden)`
});
const detailTab = (name, content) => ({
  selector: `.tab[data-tab="${name}"]`,
  ready: `Boolean(document.querySelector('.tab[data-tab="${name}"][aria-selected="true"]') && document.querySelector(${JSON.stringify(content)}))`
});

async function captureAll() {
  await capture('dashboard', 'index.html', { width: 1920, height: 1000, fullPage: true });
  await capture('dashboard-details', 'index.html', { width: 1440, height: 1000, steps: [details] });
  await capture('dashboard-details-matches', 'index.html', { width: 1440, height: 1000, steps: [details, detailTab('matches', '#detailsBody .matches-table tbody tr')] });
  await capture('dashboard-details-history', 'index.html', { width: 1440, height: 1000, steps: [details, detailTab('history', '#detailsBody .detail-table tbody tr')] });
  await capture('dashboard-light', 'index.html', { width: 1920, height: 1000, variant: 'light', fullPage: true });
  await capture('overlay-minimal', 'overlay.html', { width: 510, height: 1000, variant: 'minimal', transparent: true });
  await capture('dashboard-first-run', 'index.html', { width: 1440, height: 900, variant: 'offline' });
  await capture('dashboard-settings', 'index.html', { width: 1440, height: 1000, steps: [view('settings'), section('general')] });
  await capture('dashboard-data-sources', 'index.html', { width: 1440, height: 1100, steps: [view('settings'), section('data')] });
  await capture('dashboard-about', 'index.html', { width: 1440, height: 900, steps: [view('settings'), section('about')] });
  await capture('dashboard-help', 'index.html', { width: 1440, height: 2300, steps: [view('help')] });
  await capture('dashboard-history', 'index.html', { width: 1440, height: 1000, steps: [view('history'), {
    selector: '#historyList tr[data-match="42"]',
    ready: "Boolean(!document.getElementById('historyMatch').hidden && document.querySelector('#historyMatchPlayers tbody tr'))"
  }] });
  await capture('overlay-side', 'overlay.html', { width: 510, height: 1000, transparent: true });
  await capture('overlay-top', 'overlay.html', { width: 1180, height: 1000, variant: 'top', transparent: true });
  await capture('overlay-expanded', 'overlay.html', { width: 1480, height: 1000, variant: 'expanded', transparent: true });
  await capture('overlay-esc-menu', 'overlay.html', { width: 1280, height: 1000, variant: 'interactive', transparent: true, steps: [{
    selector: '[data-action="toggle"][data-id="76561198012345675"]',
    ready: "Boolean(document.querySelector('tr.open[data-player=\"76561198012345675\"]') && document.querySelector('.ov-detail-row .ov-panel'))"
  }] });
}

async function cleanup() {
  const failures = [];
  for (const release of [
    async () => {
      if (!server.listening) return;
      const closed = new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
      server.closeAllConnections();
      await closed;
    },
    () => stopBrowser(edge),
    () => { if (profile) rmSync(profile, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 }); }
  ]) {
    try { await release(); } catch (error) { failures.push(error); }
  }
  if (failures.length) throw new AggregateError(failures, failures.map(error => error.message).join('; '));
}

try {
  await withCleanup(async () => {
    await new Promise((resolve, reject) => {
      server.once('error', reject);
      server.listen(0, '127.0.0.1', () => { server.off('error', reject); resolve(); });
    });
    base = `http://127.0.0.1:${server.address().port}`;
    mkdirSync(output, { recursive: true });
    profile = mkdtempSync(path.join(tmpdir(), 'cs2intel-shots-'));
    edge = spawn(EDGE, ['--headless=new', '--disable-gpu', '--hide-scrollbars', '--no-first-run', '--no-default-browser-check', `--user-data-dir=${profile}`,
      '--remote-debugging-port=0', 'about:blank'], { stdio: 'ignore' });
    port = await waitForBrowser(edge, () => readFileSync(path.join(profile, 'DevToolsActivePort'), 'utf8').split('\n')[0].trim());
    await captureAll();
  }, cleanup);
} catch (error) {
  console.error(error.stack || error.message);
  process.exitCode = 1;
}
