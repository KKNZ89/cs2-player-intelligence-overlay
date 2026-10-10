import { byId, esc, setClass, setHidden, setHtml, setText } from '../lib/dom.js';
import { providerStatusText, rosterSummaryText, updateText } from '../lib/status.js';
import { renderLiveStats } from '../lib/live-stats.js';
import { icon } from '../lib/icons.js';
import { SHARED, ago, avatar, colourClass, columns as c, displayName, groups, history, isNum, issuesHtml, matchTitle, sourcesOff, csrepBlocked, playerChips, playerName, profileIndicator, rosterCounts, roundText, scoreHtml, setPriority, setPrivacy, statusIcons, teamAverages, valueOf, winChance } from '../lib/format.js';
import { renderPlayerDetails } from './details.js';
import { bindHistory, loadHistory } from './history.js';
import { api } from '../lib/bridge.js';

/** Elements by id, looked up once. @type {Record<string, any>} */
const els = new Proxy({}, { get: (cache, id) => (cache[id] ??= byId(String(id))) });
const RING = 119.4;
const TITLES = { match: 'Match', history: 'History', settings: 'Settings', diagnostics: 'Diagnostics', help: 'Help' };

let lastState = null;
let settingsPrimed = false;
let view = 'match';
let detailsId = '';
let detailsTab = 'overview';
let toastTimer = null;
let logTimer = null;
let sortKey = '';
let sortDir = -1;
let settingsSection = 'general';
/** This app's records per player (History and Notes tabs), loaded when those tabs open. */
const records = new Map();

// ---- helpers ------------------------------------------------------------------------------------

function toast(text) {
  setText(els.message, text);
  els.message.classList.toggle('visible', Boolean(text));
  clearTimeout(toastTimer);
  if (text) toastTimer = setTimeout(() => els.message.classList.remove('visible'), 6000);
}

async function busy(button, action, { spin = false } = {}) {
  button.disabled = true;
  if (spin) button.classList.add('spin');
  try { await action(); } catch (error) { toast(error.message || String(error)); }
  finally { button.disabled = false; button.classList.remove('spin'); }
}

// Static markup declares icons with data-icon; labels are wrapped so the narrow sidebar can hide them.
function decorate() {
  for (const element of /** @type {NodeListOf<HTMLElement>} */ (document.querySelectorAll('[data-icon]'))) {
    const label = element.textContent.trim();
    element.innerHTML = `${icon(element.dataset.icon, { size: element.classList.contains('nav-item') ? 18 : 15 })}${label ? `<span>${esc(label)}</span>` : ''}`;
  }
}

// ---- views --------------------------------------------------------------------------------------

function showView(name) {
  view = name;
  setText(els.pageTitle, TITLES[name]);
  for (const item of /** @type {NodeListOf<HTMLElement>} */ (document.querySelectorAll('.nav-item'))) item.classList.toggle('active', item.dataset.view === name);
  for (const section of document.querySelectorAll('.view')) setHidden(section, section.id !== `view-${name}`);
  clearInterval(logTimer);
  if (name === 'history') void loadHistory();
  if (name === 'diagnostics') {
    void loadLog();
    logTimer = setInterval(loadLog, 10000);
  }
  if (lastState) render(lastState);
}

// ---- chrome -------------------------------------------------------------------------------------

const steamLevel = steam => (steam.state === 'ready' ? 'ok' : steam.state === 'unavailable' ? 'warn' : 'off');

function renderChrome(state) {
  const g = state.gsi || {};
  const match = state.match || { state: 'offline', label: 'Waiting for CS2' };
  setText(els.appVersion, `Version ${state.version || '—'}`);
  setClass(els.matchPill, `pill ${match.state}`);
  setText(els.matchState, match.label);
  setClass(els.connGsi, `status-dot ${g.error ? 'bad' : g.connected ? 'ok' : 'off'}`);
  setText(els.connGsiText, g.error ? 'Problem' : g.connected ? 'Connected' : 'Waiting');
  const steam = state.steam || { state: 'waiting', label: 'Waiting for CS2' };
  setClass(els.connSteam, `status-dot ${steamLevel(steam)}`);
  setText(els.connSteamText, steam.label);
  els.connSteamText.title = steam.detail || '';
  setText(els.hotkeyLabel, state.setup?.hotkey || 'F8');
  const updates = state.updates || {};
  setHidden(els.installUpdateBtn, updates.status !== 'ready');
  setText(els.installUpdateBtn, `Restart to update to v${updates.version || ''}`);
  els.overlayBtn.classList.toggle('active', Boolean(state.overlayVisible));
  setHtml(els.overlayBtn, `${icon('layers', { size: 15 })}<span>${state.overlayVisible ? 'Unpin' : 'Pin'} overlay</span><kbd>${esc(state.setup?.hotkey || 'F8')}</kbd>`);
}

// ---- match view ---------------------------------------------------------------------------------

function renderHero(state) {
  const g = state.gsi || {};
  const match = state.match || {};
  setHidden(els.heroWin, !state.settings?.showWinEstimate);
  setText(els.matchLine, g.connected ? matchTitle(g) : g.error || 'Waiting for CS2');
  setText(els.liveRound, roundText(g) || (g.connected ? 'Not in a match' : 'Start CS2 and join a match'));
  setHtml(els.liveScore, scoreHtml(g));
  const win = winChance(state.prediction);
  setText(els.winValue, win.text);
  setText(els.winDetail, win.detail);
  setClass(els.winValue, `win-value ${win.percent === null ? '' : win.percent >= 55 ? 'good' : win.percent <= 45 ? 'bad' : 'even'}`);
  els.winRing.style.strokeDashoffset = String(RING * (1 - (win.percent ?? 0) / 100));
  els.winRing.parentElement.setAttribute('class', `ring side-${match.side || 'none'}`);
  els.winBar.style.width = `${win.percent ?? 50}%`;
  setClass(els.winBar.parentElement, `win-bar side-${match.side || 'none'}${win.percent === null ? ' unknown' : ''}`);
}

// Shown until CS2 reports: what is done and what is left, each with its action.
function renderGuide(state) {
  const g = state.gsi || {};
  const setup = state.setup || {};
  setHidden(els.setupGuide, Boolean(g.connected));
  // The guide covers this state; empty live stats and lobby panels would only repeat it.
  setHidden(els.liveRow, !g.connected);
  if (g.connected) return;
  const button = (action, label, name) => `<button class="btn small" data-guide="${action}">${icon(name, { size: 14 })}<span>${label}</span></button>`;
  const step = (done, title, detail, action = '') => `<li class="${done ? 'done' : ''}"><span class="step-mark">${done ? icon('check', { size: 13 }) : ''}</span><div class="step-text"><b>${title}</b><span>${detail}</span></div>${done ? '' : action}</li>`;
  const joinDetail = setup.gsiInstalled && setup.cs2Running
    ? 'Waiting for CS2 to report. If CS2 was already running when the config was installed, restart it once.'
    : 'Players and their stats then appear automatically.';
  setHtml(els.setupGuide, `<header class="panel-head"><h2>Get started</h2>${g.error ? `<span class="guide-error">${esc(g.error)}</span>` : '<span class="muted small">Four steps; the first three are one-time</span>'}</header>
    <ol class="steps">
      ${step(setup.cfgFound, 'Find CS2', setup.cfgFound ? 'CS2 install found.' : 'CS2 was not found automatically. Choose its game\\csgo\\cfg folder in Settings.', button('settings', 'Open Settings', 'settings'))}
      ${step(setup.gsiInstalled, 'Install the game state config', setup.gsiInstalled ? 'Installed with this app’s private token.' : 'Lets CS2 send your match state to this app, on this PC only.', button('gsi', 'Install', 'download'))}
      ${step(setup.cs2Running, 'Start CS2', setup.cs2Running ? 'CS2 is running.' : 'Launches through Steam as usual.', button('launch', 'Launch CS2', 'play'))}
      ${step(false, 'Join a match', joinDetail)}
    </ol>`);
}

function renderCoverage(state) {
  const coverage = state.coverage || {};
  const total = coverage.players || 0;
  const bar = (label, name) => {
    const loaded = coverage[name]?.loaded ?? 0;
    const percent = total ? Math.round((loaded / total) * 10) * 10 : 0;
    return `<div class="cov"><span>${label}</span><div class="cov-bar"><i class="w-${percent}"></i></div><b>${loaded}/${total}</b></div>`;
  };
  const counts = rosterCounts(state);
  setHtml(els.coverage, `<div class="cov-head"><b>${counts.confirmed}</b>confirmed <span class="cov-sep">·</span> <b>${counts.possible}</b>possible <span class="muted">of ${counts.max}${state.match?.mode ? ` · ${esc(state.match.mode)}` : ''}</span></div>${bar('Leetify', 'leetify')}${bar('CSRep', 'csrep')}${bar('CSStats', 'csstats')}${bar('Steam', 'steam')}`);
  updateFreshness();
}

// Team comparison from the values that exist (each average says how many players it covers) and a short
// list of noteworthy players.
const avgOf = values => (values.length ? values.reduce((a, b) => a + b, 0) / values.length : null);
function teamStats(players) {
  const pick = (fn) => players.map(fn).filter(v => isNum(v) && v > 0);
  const premier = pick(p => p.leetify?.premier);
  const level = pick(p => valueOf(p, SHARED.faceitLevel));
  const elo = pick(p => valueOf(p, SHARED.faceitElo));
  const hours = pick(p => p.steam?.hoursCs2);
  const aim = pick(p => p.leetify?.aim);
  const row = (label, values, fmt) => `<tr><td>${label}</td><td class="num">${values.length ? fmt(avgOf(values)) : '<span class="na">N/A</span>'}</td><td class="muted small num">${values.length}/${players.length}</td></tr>`;
  return row('Avg Premier', premier, v => Math.round(v).toLocaleString('en-US'))
    + row('Avg FACEIT level', level, v => v.toFixed(1))
    + row('Avg FACEIT Elo', elo, v => Math.round(v).toLocaleString('en-US'))
    + row('Avg CS2 hours', hours, v => Math.round(v).toLocaleString('en-US'))
    + row('Avg Leetify aim', aim, v => Math.round(v));
}

function notablePlayers(state) {
  const items = [];
  const met = (state.players || []).filter(p => !p.isSelf && p.localHistory?.all?.played);
  if (met.length) {
    const list = met.map(p => {
      const t = p.localHistory.all;
      return `<button class="link plain" data-action="details" data-id="${esc(p.steamId)}">${esc(displayName(p))}</button> ${t.played}× (${t.won}–${t.lost}${t.tied ? `–${t.tied}` : ''})`;
    }).join(', ');
    items.push(`<li><span class="st seen">↺</span><span>Met before: ${list}</span></li>`);
  }
  const indicators = state.settings?.profileIndicators !== false;
  for (const p of state.players || []) {
    if (p.isSelf) continue;
    const name = `<button class="link plain" data-action="details" data-id="${esc(p.steamId)}">${esc(displayName(p))}</button>`;
    if (p.strongest) items.push(`<li><span class="st strongest">★</span><span>${name} has the highest Premier in this lobby (${Math.round(p.leetify.premier).toLocaleString('en-US')}).</span></li>`);
    if (indicators && p.analysis?.indicator === 'review') items.push(`<li><span class="st review">⚑</span><span>${name}: ${esc(p.analysis.reasons.join('; '))}. <span class="muted">${p.analysis.confidence} confidence; a discrepancy, not evidence of cheating.</span></span></li>`);
    if (isNum(p.leetify?.aim) && p.leetify.aim >= 90 && (p.leetify?.totalMatches || 0) >= (state.settings?.minSampleMatches ?? 30)) items.push(`<li><span class="st">◎</span><span>${name}: Leetify aim ${Math.round(p.leetify.aim)} over ${p.leetify.totalMatches.toLocaleString('en-US')} matches.</span></li>`);
  }
  return items;
}

function renderLobbySummary(state) {
  const players = state.players || [];
  setHidden(els.lobbySummary, !players.length);
  if (!players.length) return;
  const own = groups(state, 64);
  const team = own.find(group => group.side === 'team');
  const enemy = own.find(group => group.side === 'enemy');
  const ffa = state.match?.teams === false;
  const comparison = ffa || !team || !enemy
    ? `<p class="muted small">${ffa ? 'Free-for-all: no teams to compare.' : 'Team comparison appears once players are on both sides (spectate teammates, or mark players as Team or Enemy).'}</p><table class="compare"><thead><tr><th></th><th class="num">Lobby</th><th class="num">With data</th></tr></thead><tbody>${teamStats(players.filter(p => !p.isSelf))}</tbody></table>`
    : `<div class="compare-cols"><div><h4 class="ally">Your team</h4><table class="compare"><tbody>${teamStats(team.players)}</tbody></table></div><div><h4 class="opponent">Opponents</h4><table class="compare"><tbody>${teamStats(enemy.players)}</tbody></table></div></div>`;
  const notable = notablePlayers(state);
  setHtml(els.lobbySummary, `<header class="panel-head"><h2>Lobby summary</h2><span class="muted small">${esc(matchTitle(state.gsi || {}))} · averages of the values available</span></header>
    <div class="summary-grid"><div>${comparison}</div><div><h4>Notable players</h4>${notable.length ? `<ul class="notable">${notable.join('')}</ul>` : '<p class="muted small">Nothing stands out yet. Players met before, the strongest by Premier and flagged profiles appear here.</p>'}</div></div>`);
}

function updateFreshness() {
  setText(els.freshness, `Updated ${ago(lastState?.coverage?.lastFetchedAt)}`);
}

function sideControl(p, group) {
  if (p.isSelf || group.ffa) return '';
  const id = esc(p.steamId);
  if (group.side === 'unknown') {
    return `<div class="assign"><button class="assign-btn team" data-action="side" data-side="team" data-id="${id}">Teammate</button><button class="assign-btn enemy" data-action="side" data-side="enemy" data-id="${id}">Opponent</button></div>`;
  }
  // Clicking the active side again clears it.
  const option = (side, label) => `<button class="seg ${side} ${p.side === side ? 'active' : ''}" data-action="side" data-side="${p.side === side ? '' : side}" data-id="${id}" aria-pressed="${p.side === side}" title="${p.side === side ? 'Click to unassign' : `Mark as ${label.toLowerCase()}`}">${label}</button>`;
  return `<div class="segmented" role="group" aria-label="Side">${option('team', 'Team')}${option('enemy', 'Enemy')}</div>`;
}

const indicatorsOn = () => lastState?.settings?.profileIndicators !== false;

function playerRow(p, group) {
  const inferred = group.inferred.has(p.steamId);
  const ring = group.ffa ? '' : group.side;
  const chips = playerChips(p, { inferred, state: lastState || {} });
  return `<tr class="${p.isSelf ? 'self' : ''}" data-player="${esc(p.steamId)}">
    <td><div class="player-cell"><span class="avatar-wrap ${ring}${colourClass(p)}">${avatar(p, 32)}</span><div><button class="player-link" data-action="details" data-id="${esc(p.steamId)}">${playerName(p)}</button>${statusIcons(p, { indicators: indicatorsOn() })}${chips ? `<div class="chips">${chips}</div>` : ''}${issuesHtml(p)}</div></div></td>
    <td class="num">${c.premier(p)}</td><td class="num">${c.kd(p)}</td><td class="num">${c.winRate(p)}</td><td class="num">${c.faceit(p)}</td>
    <td class="num">${c.csrep(p)}</td><td class="num">${c.timeToDamage(p)}</td><td class="num">${c.hours(p)}</td>
    ${indicatorsOn() ? `<td>${profileIndicator(p)}</td>` : ''}
    <td class="history">${history(p)}</td>
    <td class="side-cell">${sideControl(p, group)}</td>
    <td class="row-actions"><button class="icon-btn" data-action="refresh" data-id="${esc(p.steamId)}" title="Refresh this player" aria-label="Refresh">${icon('refresh', { size: 15 })}</button><button class="icon-btn" data-action="details" data-id="${esc(p.steamId)}" title="Details" aria-label="Details">${icon('chevron', { size: 15 })}</button>${p.isSelf ? '' : `<button class="icon-btn danger" data-action="remove" data-id="${esc(p.steamId)}" title="Remove from this match" aria-label="Remove">${icon('close', { size: 15 })}</button>`}</td>
  </tr>`;
}

// Sortable columns: the value each sorts by (missing values go last).
const SORT = {
  premier: p => p.leetify?.premier,
  kd: p => valueOf(p, SHARED.kd),
  win: p => valueOf(p, SHARED.win),
  faceit: p => valueOf(p, SHARED.faceitLevel),
  csrep: p => p.csrep?.trust,
  ttd: p => valueOf(p, SHARED.ttd),
  hours: p => p.steam?.hoursCs2,
  profile: p => ({ review: 2, normal: 1, insufficient: 0 })[p.analysis?.indicator],
  seen: p => p.localHistory?.all?.played
};

function sorted(players) {
  if (!sortKey || !SORT[sortKey]) return players;
  const value = p => SORT[sortKey](p);
  return [...players].sort((a, b) => {
    const [x, y] = [value(a), value(b)];
    if (!isNum(x)) return isNum(y) ? 1 : 0;
    if (!isNum(y)) return -1;
    return (x - y) * sortDir;
  });
}

// Fixed column widths keep the team tables aligned with each other.
function teamHead() {
  const th = (label, key, cls = 'num') => `<th class="${cls} sortable${sortKey === key ? ` sorted ${sortDir > 0 ? 'asc' : 'desc'}` : ''}" data-sort="${key}" aria-sort="${sortKey === key ? (sortDir > 0 ? 'ascending' : 'descending') : 'none'}" tabindex="0">${label}</th>`;
  const profile = indicatorsOn();
  return `<colgroup><col class="c-player"><col class="c-premier"><col class="c-num"><col class="c-num"><col class="c-num"><col class="c-num"><col class="c-num"><col class="c-num">${profile ? '<col class="c-profile">' : ''}<col class="c-history"><col class="c-side"><col class="c-actions"></colgroup>
    <thead><tr><th>Player</th>${th('Premier', 'premier')}${th('K/D', 'kd')}${th('Win %', 'win')}${th('FACEIT', 'faceit')}${th('CSRep', 'csrep')}${th('TTD', 'ttd')}${th('Hours', 'hours')}${profile ? th('Profile', 'profile', '') : ''}${th('Played before', 'seen', '')}<th>Side</th><th></th></tr></thead>`;
}

// Why the player list is empty, in the user's terms.
function emptyReason(state) {
  const g = state.gsi || {};
  const match = state.match || {};
  if (!g.connected) return ['Waiting for CS2', 'Follow the steps above. Players appear once CS2 reports a match.'];
  if (match.state === 'idle' || match.state === 'over') return ['Not in a match', 'Players appear automatically when you join one. You can also add them above.'];
  return ['Looking for players', 'Reading Steam’s recently played list for this match; this takes a few seconds.'];
}

function renderTeams(state) {
  if (!(state.players || []).length) {
    const [title, text] = emptyReason(state);
    setHtml(els.playersBody, `<div class="empty panel">${icon('users', { size: 40 })}<h3>${title}</h3><p class="muted">${text}</p></div>`);
    return;
  }
  const teamSize = state.match?.teamSize || 0;
  setHtml(els.playersBody, groups(state, 64).map(group => {
    const averages = teamAverages(group.players).split(' · ').filter(Boolean).map(text => `<span class="chip">${esc(text)}</span>`).join('');
    const count = teamSize && group.side !== 'unknown' ? `${group.players.length}/${teamSize}` : String(group.players.length);
    return `<section class="team ${group.side} ${group.teamSide ? `side-${group.teamSide}` : ''}">
      <header class="team-header"><span class="team-dot"></span><h2 title="${esc(group.description || '')}">${group.title}</h2><span class="team-avg">${averages}</span><span class="team-count">${count}</span></header>
      <div class="table-wrap"><table class="table players">${teamHead()}<tbody>${sorted(group.players).map(p => playerRow(p, group)).join('')}</tbody></table></div>
    </section>`;
  }).join(''));
}

let detailsOpener = null;

function renderDetails() {
  const player = lastState?.players?.find(p => p.steamId === detailsId);
  const wasOpen = !els.details.hidden;
  setHidden(els.details, !detailsId || !player);
  if (!player) {
    if (wasOpen && detailsOpener?.isConnected) detailsOpener.focus();
    if (wasOpen) detailsOpener = null;
    return;
  }
  if (!wasOpen) {
    detailsOpener = /** @type {HTMLElement | null} */ (document.activeElement);
    requestAnimationFrame(() => els.details.focus());
  }
  if (['history', 'notes'].includes(detailsTab) && !records.has(detailsId)) loadRecord(detailsId);
  const { identity, body } = renderPlayerDetails(player, lastState, detailsTab, records.get(detailsId));
  markSelected(/** @type {NodeListOf<HTMLElement>} */ (document.querySelectorAll('.tab')), tab => tab.dataset.tab === detailsTab);
  setHtml(els.detailsIdentity, identity);
  setHtml(els.detailsBody, body);
}

async function loadRecord(id) {
  records.set(id, null);
  try { records.set(id, await api.playerHistory(id)); } catch (error) { records.delete(id); toast(error.message || String(error)); }
  if (detailsId === id) renderDetails();
}

// Tab lists: arrow keys (and Home/End) move between tabs and select them, as in native tab controls.
function roveTabs(list, select, vertical = false) {
  list.addEventListener('keydown', event => {
    const tabs = /** @type {HTMLElement[]} */ ([...list.querySelectorAll('[role="tab"]')]);
    const index = tabs.indexOf(/** @type {HTMLElement} */ (document.activeElement));
    if (index < 0) return;
    const keys = vertical ? { ArrowUp: -1, ArrowDown: 1 } : { ArrowLeft: -1, ArrowRight: 1 };
    let next = null;
    if (event.key in keys) next = (index + keys[event.key] + tabs.length) % tabs.length;
    else if (event.key === 'Home') next = 0;
    else if (event.key === 'End') next = tabs.length - 1;
    if (next === null) return;
    event.preventDefault();
    tabs[next].focus();
    select(tabs[next]);
  });
}

function markSelected(tabs, isSelected) {
  for (const tab of tabs) {
    const selected = isSelected(tab);
    tab.classList.toggle('active', selected);
    tab.setAttribute('aria-selected', String(selected));
    tab.tabIndex = selected ? 0 : -1;
  }
}

// ---- settings view ------------------------------------------------------------------------------

const COLUMN_LABELS = { hours: 'Hours', premier: 'Premier', faceit: 'FACEIT', aim: 'Aim', ttd: 'TTD', hs: 'HS %', kd: 'K/D', win: 'Win %', csrep: 'CSRep', profile: 'Profile' };
let columnOrder = [];

function renderColumnList(enabled) {
  const rest = Object.keys(COLUMN_LABELS).filter(key => !enabled.includes(key));
  columnOrder = [...enabled, ...rest];
  setHtml(els.overlayColumnList, columnOrder.map((key, i) => `<li><label class="check"><input type="checkbox" data-column="${key}" ${enabled.includes(key) ? 'checked' : ''}><span>${COLUMN_LABELS[key]}</span></label>
    <span class="column-moves"><button class="icon-btn small" type="button" data-move="-1" data-column="${key}" aria-label="Move up" ${i ? '' : 'disabled'}>↑</button><button class="icon-btn small" type="button" data-move="1" data-column="${key}" aria-label="Move down" ${i < columnOrder.length - 1 ? '' : 'disabled'}>↓</button></span></li>`).join(''));
}

const SOURCE_LABELS = { faceit: 'FACEIT (API)', leetify: 'Leetify (API)', csstats: 'CSStats (page text)', csrep: 'CSRep (page text)' };
let sourceOrder = [];

function renderPriorityList(order) {
  sourceOrder = [...order];
  setHtml(els.sourcePriorityList, sourceOrder.map((key, i) => `<li><span><b class="rank-no">${i + 1}</b> ${SOURCE_LABELS[key] || esc(key)}</span>
    <span class="column-moves"><button class="icon-btn small" type="button" data-source-move="-1" data-source="${key}" aria-label="Move ${SOURCE_LABELS[key]} up" ${i ? '' : 'disabled'}>↑</button><button class="icon-btn small" type="button" data-source-move="1" data-source="${key}" aria-label="Move ${SOURCE_LABELS[key]} down" ${i < sourceOrder.length - 1 ? '' : 'disabled'}>↓</button></span></li>`).join(''));
}

function chosenColumns() {
  const checked = new Set([...els.overlayColumnList.querySelectorAll('input[data-column]:checked')].map(box => box.dataset.column));
  return columnOrder.filter(key => checked.has(key));
}

function showSection(name) {
  settingsSection = name;
  markSelected(/** @type {NodeListOf<HTMLElement>} */ (document.querySelectorAll('.settings-tab')), tab => tab.dataset.section === name);
  for (const panel of document.querySelectorAll('.settings-section')) setHidden(panel, panel.dataset.section !== name);
  if (name === 'database') void loadDbStats();
}

async function loadDbStats() {
  try {
    const stats = await api.historyStats();
    const size = stats.bytes > 1048576 ? `${(stats.bytes / 1048576).toFixed(1)} MB` : `${Math.max(1, Math.round(stats.bytes / 1024))} KB`;
    setHtml(els.dbStats, [['Matches', stats.matches], ['Players met', stats.players], ['Recorded values', stats.snapshots], ['Notes', stats.notes], ['File', `${stats.file} (${size})`], ...(stats.error ? [['Problem', stats.error]] : [])]
      .map(([k, v]) => `<dt>${esc(k)}</dt><dd>${esc(String(v))}</dd>`).join(''));
  } catch (error) { setHtml(els.dbStats, `<dt>Error</dt><dd>${esc(error.message || String(error))}</dd>`); }
}

function primeSettings(state) {
  const s = state.settings || {};
  els.sdkPath.value = s.steamworksSdkPath || '';
  els.cfgPath.value = s.cs2CfgPath || '';
  els.consolePath.value = s.cs2ConsoleLogPath || '';
  els.updateSource.value = s.updateSource || '';
  els.updateSource.placeholder = s.defaultUpdateSource?.startsWith('https://github.com/') ? 'GitHub releases (default)' : s.defaultUpdateSource ? `Default: ${s.defaultUpdateSource}` : 'Release folder or https:// URL';
  els.csstatsEnabled.checked = s.csstatsEnabled === true;
  els.csrepPagesEnabled.checked = s.csrepPagesEnabled === true;
  els.launchCs2OnStart.checked = s.launchCs2OnStart !== false;
  els.holdTab.checked = (s.overlayTrigger || 'hold-tab') === 'hold-tab';
  els.escInteractive.checked = s.escInteractive !== false;
  els.scoreboardColours.checked = s.scoreboardColours === true;
  els.autoInstallUpdates.checked = s.autoInstallUpdates !== false;
  els.closeToTray.checked = s.closeToTray !== false;
  els.privacyMode.checked = s.privacyMode === true;
  els.profileIndicators.checked = s.profileIndicators !== false;
  els.showWinEstimate.checked = s.showWinEstimate === true;
  els.notifications.checked = s.notifications !== false;
  for (const box of /** @type {NodeListOf<HTMLInputElement>} */ (document.querySelectorAll('input[name="notifyType"]'))) box.checked = (s.notifyTypes || []).includes(box.value);
  for (const radio of /** @type {NodeListOf<HTMLInputElement>} */ (document.querySelectorAll('input[name="theme"]'))) radio.checked = radio.value === (s.theme || 'dark');
  for (const radio of /** @type {NodeListOf<HTMLInputElement>} */ (document.querySelectorAll('input[name="overlayStyle"]'))) radio.checked = radio.value === (s.overlayStyle || 'standard');
  renderColumnList(s.overlayColumns || []);
  renderPriorityList(s.sourcePriority || ['faceit', 'leetify', 'csstats', 'csrep']);
  els.startWithWindows.checked = s.startWithWindows === true;
  els.overlayHotkey.value = s.overlayHotkey || 'F8';
  els.interactHotkey.value = s.interactHotkey || 'Shift+F8';
  for (const radio of /** @type {NodeListOf<HTMLInputElement>} */ (document.querySelectorAll('input[name="overlayLayout"]'))) radio.checked = radio.value === (s.overlayLayout || 'right');
  for (const box of /** @type {NodeListOf<HTMLInputElement>} */ (document.querySelectorAll('input[name="group"]'))) box.checked = (s.expandedGroups || []).includes(box.value);
  for (const key of ['overlayX', 'overlayY', 'overlayScale', 'overlayOpacity', 'autoRetryMinutes', 'minSampleMatches', 'retentionDays', 'cacheMinutes']) els[key].value = s[key];
  rangeLabels();
  settingsPrimed = true;
}

function rangeLabels() {
  setText(els.overlayScaleValue, `${Math.round(Number(els.overlayScale.value) * 100)}%`);
  setText(els.overlayOpacityValue, `${Math.round(Number(els.overlayOpacity.value) * 100)}%`);
}

function renderDisplays(state) {
  const html = '<option value="">Primary display</option>' + (state.displays || []).map(d => `<option value="${esc(d.id)}">${esc(d.label)}</option>`).join('');
  if (els.overlayDisplay.dataset.html === html) return;
  const selected = els.overlayDisplay.value || state.settings?.overlayDisplay || '';
  setHtml(els.overlayDisplay, html);
  els.overlayDisplay.value = selected;
}

function statusLines(state) {
  const g = state.gsi || {};
  const setup = state.setup || {};
  return [
    ['GSI', g.error || (g.connected ? `Connected${g.notice ? ` (${g.notice})` : ''}` : 'Disconnected')],
    ['Steam', state.steam ? `${state.steam.label}: ${state.steam.detail}` : '—'],
    ['CSRep', setup.csrep || 'unknown'],
    ['CSStats', setup.csstats || 'unknown'],
    ['Overlay shortcut', `${setup.hotkey || 'F8'}${setup.shortcutReady ? '' : ' (not registered)'}`],
    ['Clickable overlay shortcut', `${setup.interactHotkey || 'Shift+F8'}${setup.interactReady ? '' : ' (not registered)'}`],
    ['Overlay', `${state.overlayVisible ? 'pinned' : 'not pinned'}${state.overlayMouseNavigation ? ' · mouse navigation (focused)' : setup.menuOpen ? ' · Esc menu open (clickable)' : ''}`],
    ['Game mode', state.match?.mode || '—'],
    ['Version', state.version || '?'],
    ['Updates', updateText(state.updates)],
    ['Diagnostics file', setup.diagnosticsFile || '']
  ];
}

function statusChips(state) {
  const g = state.gsi || {};
  const setup = state.setup || {};
  const s = state.settings || {};
  const steam = state.steam || {};
  const chip = (label, value, level, title = '') => `<span class="status-chip" title="${esc(title)}"><span class="status-dot ${level}"></span><span>${esc(label)}</span><b>${esc(value)}</b></span>`;
  return [
    chip('Game state', g.error ? 'Problem' : g.connected ? 'Connected' : 'Waiting for CS2', g.error ? 'bad' : g.connected ? 'ok' : 'off', g.error || g.notice || ''),
    chip('Steam', steam.label || 'Waiting for CS2', steamLevel(steam), steam.detail || ''),
    chip('Shortcut', setup.hotkey || 'F8', setup.shortcutReady ? 'ok' : 'warn', setup.shortcutReady ? 'Registered' : 'Not registered: another app may use it'),
    chip('CSRep', s.hasCsrepApiKey ? 'API key' : !s.csrepPagesEnabled ? 'Off' : setup.csrepSignIn?.signedIn ? 'Signed in' : 'Public pages', s.hasCsrepApiKey || s.csrepPagesEnabled ? 'ok' : 'off', 'Settings → Data sources'),
    chip('CSStats', !s.csstatsEnabled ? 'Off' : !setup.csstatsSignIn ? 'On' : setup.csstatsSignIn.signedIn ? 'Signed in' : 'Not signed in', !s.csstatsEnabled ? 'off' : setup.csstatsSignIn?.signedIn === false ? 'warn' : 'ok', 'Settings → Data sources')
  ].join('');
}

const badge = (level, text, detail = '') => `<span class="status-dot ${level}"></span><b>${esc(text)}</b>${detail ? ` <span class="muted">${esc(detail)}</span>` : ''}`;

function renderSourceStatus(state) {
  const s = state.settings || {};
  const prints = s.keyFingerprints || {};
  for (const [id, key, has] of [['steamKeyStatus', 'steamWebApiKey', s.hasSteamWebApiKey], ['faceitKeyStatus', 'faceitApiKey', s.hasFaceitApiKey], ['csrepKeyStatus', 'csrepApiKey', s.hasCsrepApiKey]]) {
    setHtml(els[id], has ? badge('ok', 'Key saved', prints[key] ? `SHA-256 ${prints[key]}` : '') : badge('off', 'No key'));
  }
  const signIn = state.setup?.csstatsSignIn;
  let status;
  if (!s.csstatsEnabled) status = badge('off', 'Page reading off');
  else if (!signIn) status = badge('off', 'Not checked yet', 'shown after the first lookup');
  else if (signIn.signedIn) status = badge('ok', 'Signed in', `checked ${ago(signIn.checkedAt)}`);
  else status = badge('warn', 'Not signed in', `pages had no stats ${ago(signIn.checkedAt)}`);
  setHtml(els.csstatsStatus, status);
  const csrep = state.setup?.csrepSignIn;
  let csrepStatus;
  if (s.hasCsrepApiKey) csrepStatus = badge('ok', 'Not needed', 'the API key is used');
  else if (!s.csrepPagesEnabled) csrepStatus = badge('off', 'Page reading off');
  else if (!csrep) csrepStatus = badge('off', 'Not checked yet', 'shown after the first lookup');
  else if (csrep.signedIn) csrepStatus = badge('ok', 'Signed in', `checked ${ago(csrep.checkedAt)}`);
  else csrepStatus = badge('warn', 'Not signed in', `your page had no stats ${ago(csrep.checkedAt)}`);
  setHtml(els.csrepStatus, csrepStatus);
  const csrepLabel = csrep?.signedIn ? 'Open CSRep' : 'Sign in to CSRep';
  if (els.csrepLoginBtn.lastChild?.textContent !== csrepLabel) els.csrepLoginBtn.lastChild.textContent = csrepLabel;
  const label = signIn?.signedIn ? 'Open CSStats' : 'Sign in to CSStats';
  if (els.csstatsLoginBtn.lastChild?.textContent !== label) els.csstatsLoginBtn.lastChild.textContent = label;
}

function renderSettings(state) {
  renderDisplays(state);
  if (!settingsPrimed) primeSettings(state);
  const s = state.settings || {};
  els.csrepKey.placeholder = s.hasCsrepApiKey ? 'Saved key: leave blank to keep' : "Optional: uses CSRep's API";
  els.steamKey.placeholder = s.hasSteamWebApiKey ? 'Saved key: leave blank to keep' : 'Needed only for CS2 hours and game bans';
  els.faceitKey.placeholder = s.hasFaceitApiKey ? 'Saved key: leave blank to keep' : 'Adds FACEIT matches, K/D, HS % and profile links';
  renderSourceStatus(state);
  setText(els.aboutVersion, `CS2 Player Intel ${state.version || ''}`);
  setHtml(els.setupStatus, statusChips(state));
  const notices = [state.setup?.gsiConfig, state.setup?.game, ...(state.setup?.warnings || [])].filter(Boolean);
  setHidden(els.setupNotices, !notices.length);
  setHtml(els.setupNotices, notices.map(text => `<p>${icon('alert', { size: 14 })}<span>${esc(text)}</span></p>`).join(''));
  setText(els.updateStatus, updateText(state.updates));
  setHidden(els.checkUpdatesBtn, state.updates?.status === 'disabled');
}

// Records a key combination as an accelerator, for example "Ctrl+Shift+I".
const KEY_NAMES = { ' ': 'Space', ArrowUp: 'Up', ArrowDown: 'Down', ArrowLeft: 'Left', ArrowRight: 'Right', '+': 'Plus' };
function acceleratorFrom(event) {
  if (['Control', 'Shift', 'Alt', 'Meta'].includes(event.key)) return '';
  const key = KEY_NAMES[event.key] || (/^F\d{1,2}$/.test(event.key) ? event.key : event.key.length === 1 ? event.key.toUpperCase() : event.key);
  return [event.ctrlKey && 'Ctrl', event.altKey && 'Alt', event.shiftKey && 'Shift', key].filter(Boolean).join('+');
}

// ---- diagnostics view ---------------------------------------------------------------------------

function renderDiagnostics(state) {
  setHtml(els.diagStatus, statusLines(state).map(([k, v]) => `<dt>${esc(k)}</dt><dd>${esc(v)}</dd>`).join(''));
  const cell = result => {
    const status = result?.stale ? 'stale' : result?.status || 'pending';
    const level = status === 'ok' ? 'ok' : ['pending', 'not-found', 'disabled', 'self', 'no-own-history'].includes(status) ? 'off' : 'warn';
    return `<td><span class="status-dot ${level}"></span>${esc(result?.stale ? `stale: ${result.staleReason}` : providerStatusText(result))}</td>`;
  };
  setHtml(els.diagProviders, (state.players || []).map(p => `<tr><td>${playerName(p)}</td>${cell(p.leetify)}${cell(p.csrep)}${cell(p.csstats)}${cell(p.steam)}${cell(p.history)}</tr>`).join('')
    || '<tr><td colspan="6" class="muted">No players yet.</td></tr>');
}

/** Copies the last match's log, folded for pasting; shows it in the log box if the clipboard refuses. */
async function copyReport() {
  const report = await api.matchReport();
  try {
    await navigator.clipboard.writeText(report);
    toast(`Copied the last match's log (${report.split('\n').length} lines).`);
  } catch {
    setText(els.diagLog, report);
    toast('Could not use the clipboard; the report is in the log box to copy.');
  }
}

async function loadLog() {
  try {
    const lines = await api.recentDiagnostics();
    setText(els.diagLog, lines.length ? lines.join('\n') : 'No problems logged.');
  } catch (error) { setText(els.diagLog, `Could not read the log: ${error.message}`); }
}

// ---- render -------------------------------------------------------------------------------------

// A source switched off explains many N/A values; say so once, with a way to turn it on. Hiding the
// notice is remembered on this PC.
const NOTICE_KEY = 'sourcesNoticeHidden';
function noticeHidden() {
  try { return localStorage.getItem(NOTICE_KEY) === '1'; } catch { return false; }
}

function renderSourcesNotice(state) {
  const players = state.players || [];
  const off = sourcesOff(state.settings);
  const parts = [];
  if (off && players.length && !noticeHidden()) {
    const missing = [off.includes('CSStats') && 'K/D, ADR and HS %', off.includes('CSRep') && 'the CSRep Trust Score'].filter(Boolean).join(' and ');
    parts.push(`<p>${icon('alert', { size: 14 })}<span>${off} ${off.includes(' and ') ? 'are' : 'is'} off, so ${missing} often show N/A. Reading their pages is opt-in: check their terms, then turn ${off.includes(' and ') ? 'them' : 'it'} on.</span></p>
    <div class="button-row"><button class="btn small" type="button" data-notice="open">Open Data sources</button><button class="btn ghost small" type="button" data-notice="hide">Don't show again</button></div>`);
  }
  const blocked = csrepBlocked(players);
  if (blocked) {
    parts.push(`<p>${icon('alert', { size: 14 })}<span>CSRep asked for a security check, so ${blocked === 1 ? '1 player has' : `${blocked} players have`} no CSRep data. Complete the check once in the window that opens, then close it: every blocked player is looked up again.</span></p>
    <div class="button-row"><button class="btn small" type="button" data-notice="csrep-verify">Complete CSRep check</button></div>`);
  }
  setHidden(els.sourcesNotice, !parts.length);
  setHtml(els.sourcesNotice, parts.join(''));
}

function render(state) {
  lastState = state;
  document.documentElement.dataset.theme = state.settings?.theme === 'light' ? 'light' : 'dark';
  setPrivacy(state.settings?.privacyMode === true, state.players || []);
  setPriority(state.settings?.sourcePriority);
  renderChrome(state);
  // Settings inputs are primed once; each view renders only while open.
  renderSettings(state);
  if (view === 'match') {
    renderHero(state);
    renderGuide(state);
    renderLiveStats(els.liveStats, state.gsi || {}, { tiles: true });
    renderCoverage(state);
    renderSourcesNotice(state);
    renderLobbySummary(state);
    const last = state.lastMatch;
    setText(els.rosterHint, last
      ? `${rosterSummaryText(state)}. From your last match${last.map ? ` on ${last.map.replace(/^(de|cs)_/, '')}` : ''}; they clear when your next match starts.`
      : state.rosterSelection === 'selected'
        ? `${rosterSummaryText(state)}. Showing the players you chose; the automatic list returns when the match ends.`
        : `${rosterSummaryText(state)}. Found automatically from Steam while CS2 runs.`);
    renderTeams(state);
  }
  if (view === 'diagnostics') renderDiagnostics(state);
  renderDetails();
}

// ---- events -------------------------------------------------------------------------------------

function handleAction(event) {
  const button = /** @type {HTMLButtonElement | null} */ (event.target.closest('button[data-action]'));
  if (!button) return;
  const { action, id } = button.dataset;
  if (action === 'details') { detailsId = id; detailsTab = 'overview'; renderDetails(); }
  else if (action === 'profile') busy(button, () => api.openProfile(button.dataset.provider, id));
  else if (action === 'refresh') busy(button, () => api.refreshPlayer(id), { spin: true });
  else if (action === 'remove') busy(button, async () => { await api.removePlayer(id); if (detailsId === id) { detailsId = ''; renderDetails(); } });
  else if (action === 'side') busy(button, () => api.setPlayerSide(id, button.dataset.side));
  else if (action === 'note-add') {
    const text = /** @type {HTMLTextAreaElement} */ (byId('noteText')).value;
    busy(button, async () => { await api.addNote(id, text); records.delete(id); await loadRecord(id); toast('Note saved.'); });
  } else if (action === 'note-delete') {
    busy(button, async () => { await api.deleteNote(Number(button.dataset.note)); records.delete(detailsId); await loadRecord(detailsId); toast('Note deleted.'); });
  }
}

// Settings save as they change: switches and choices at once, text fields when you leave them, sliders
// when you let go. A rejected value is reported and the form returns to the saved settings.
let saveQueue = Promise.resolve(true);
let saveStateTimer = null;

function setSaveState(text, level = '') {
  setText(els.saveState, text);
  setClass(els.saveState, `save-state ${level}`);
  clearTimeout(saveStateTimer);
  if (level === 'ok') saveStateTimer = setTimeout(() => setSaveState('Changes save automatically'), 2500);
}

function save(input) {
  setSaveState('Saving…');
  saveQueue = saveQueue.then(async () => {
    try {
      await api.saveSettings(input);
      setSaveState('Saved', 'ok');
      return true;
    } catch (error) {
      setSaveState(error.message || String(error), 'bad');
      if (lastState) primeSettings(lastState);
      return false;
    }
  });
  return saveQueue;
}

function bindSettings() {
  const on = (element, event, handler) => element.addEventListener(event, handler);
  for (const key of ['escInteractive', 'scoreboardColours', 'autoInstallUpdates', 'csstatsEnabled', 'csrepPagesEnabled', 'launchCs2OnStart', 'closeToTray', 'startWithWindows']) on(els[key], 'change', () => save({ [key]: els[key].checked }));
  on(els.holdTab, 'change', () => save({ overlayTrigger: els.holdTab.checked ? 'hold-tab' : 'f8' }));
  for (const radio of /** @type {NodeListOf<HTMLInputElement>} */ (document.querySelectorAll('input[name="overlayLayout"]'))) on(radio, 'change', () => save({ overlayLayout: radio.value }));
  for (const box of /** @type {NodeListOf<HTMLInputElement>} */ (document.querySelectorAll('input[name="group"]'))) {
    on(box, 'change', () => save({ expandedGroups: [.../** @type {NodeListOf<HTMLInputElement>} */ (document.querySelectorAll('input[name="group"]:checked'))].map(b => b.value) }));
  }
  on(els.overlayDisplay, 'change', () => save({ overlayDisplay: els.overlayDisplay.value }));
  for (const key of ['overlayScale', 'overlayOpacity']) {
    on(els[key], 'input', rangeLabels);
    on(els[key], 'change', () => save({ [key]: Number(els[key].value) }));
  }
  for (const key of ['overlayX', 'overlayY', 'autoRetryMinutes']) on(els[key], 'change', () => save({ [key]: els[key].value }));
  for (const [id, key] of [['cfgPath', 'cs2CfgPath'], ['consolePath', 'cs2ConsoleLogPath'], ['sdkPath', 'steamworksSdkPath'], ['updateSource', 'updateSource']]) on(els[id], 'change', () => save({ [key]: els[id].value }));
  for (const key of ['privacyMode', 'profileIndicators', 'showWinEstimate', 'notifications']) on(els[key], 'change', () => save({ [key]: els[key].checked }));
  for (const box of /** @type {NodeListOf<HTMLInputElement>} */ (document.querySelectorAll('input[name="notifyType"]'))) {
    on(box, 'change', () => save({ notifyTypes: [.../** @type {NodeListOf<HTMLInputElement>} */ (document.querySelectorAll('input[name="notifyType"]:checked'))].map(b => b.value) }));
  }
  for (const name of ['theme', 'overlayStyle']) {
    for (const radio of /** @type {NodeListOf<HTMLInputElement>} */ (document.querySelectorAll(`input[name="${name}"]`))) on(radio, 'change', () => save({ [name]: radio.value }));
  }
  for (const key of ['minSampleMatches', 'retentionDays', 'cacheMinutes']) on(els[key], 'change', () => save({ [key]: els[key].value }));
  on(els.sourcePriorityList, 'click', event => {
    const button = /** @type {HTMLButtonElement | null} */ (/** @type {Element} */ (event.target).closest('button[data-source-move]'));
    if (!button) return;
    const from = sourceOrder.indexOf(button.dataset.source);
    const to = from + Number(button.dataset.sourceMove);
    if (to < 0 || to >= sourceOrder.length) return;
    [sourceOrder[from], sourceOrder[to]] = [sourceOrder[to], sourceOrder[from]];
    renderPriorityList(sourceOrder);
    // Keep keyboard focus on the moved source's matching button.
    els.sourcePriorityList.querySelector(`button[data-source="${button.dataset.source}"][data-source-move="${button.dataset.sourceMove}"]:not([disabled])`)?.focus();
    void save({ sourcePriority: sourceOrder });
  });
  on(els.overlayColumnList, 'change', () => { const chosen = chosenColumns(); if (chosen.length) void save({ overlayColumns: chosen }); else { toast('Keep at least one column.'); renderColumnList(lastState?.settings?.overlayColumns || []); } });
  on(els.overlayColumnList, 'click', event => {
    const button = /** @type {HTMLButtonElement | null} */ (/** @type {Element} */ (event.target).closest('button[data-move]'));
    if (!button) return;
    const from = columnOrder.indexOf(button.dataset.column);
    const to = from + Number(button.dataset.move);
    if (to < 0 || to >= columnOrder.length) return;
    const enabled = chosenColumns();
    [columnOrder[from], columnOrder[to]] = [columnOrder[to], columnOrder[from]];
    const ordered = columnOrder.filter(key => enabled.includes(key));
    renderColumnList(ordered);
    void save({ overlayColumns: ordered });
  });
  for (const tab of /** @type {NodeListOf<HTMLElement>} */ (document.querySelectorAll('.settings-tab'))) on(tab, 'click', () => showSection(tab.dataset.section));
  for (const [id, key] of [['csrepKey', 'csrepApiKey'], ['steamKey', 'steamWebApiKey'], ['faceitKey', 'faceitApiKey']]) {
    on(els[id], 'change', async () => {
      const value = els[id].value.trim();
      if (value && await save({ [key]: value })) els[id].value = '';
    });
  }
}

function bind() {
  bindHistory(message => toast(message));
  els.sourcesNotice.addEventListener('click', event => {
    const button = /** @type {HTMLElement | null} */ (/** @type {Element} */ (event.target).closest('button[data-notice]'));
    if (!button) return;
    if (button.dataset.notice === 'open') {
      showView('settings');
      showSection('data');
    } else if (button.dataset.notice === 'csrep-verify') {
      busy(/** @type {HTMLButtonElement} */ (button), () => api.verifyCsRep());
    } else {
      try { localStorage.setItem(NOTICE_KEY, '1'); } catch { /* the notice just shows again next time */ }
      if (lastState) renderSourcesNotice(lastState);
    }
  });
  for (const item of /** @type {NodeListOf<HTMLElement>} */ (document.querySelectorAll('.nav-item'))) item.addEventListener('click', () => showView(item.dataset.view));
  els.playersBody.addEventListener('click', handleAction);
  els.lobbySummary.addEventListener('click', handleAction);
  els.details.addEventListener('click', handleAction);
  els.detailsClose.addEventListener('click', () => { detailsId = ''; renderDetails(); });
  els.detailsRefresh.addEventListener('click', () => busy(els.detailsRefresh, () => api.refreshPlayer(detailsId), { spin: true }));
  const selectDetailsTab = tab => { detailsTab = tab.dataset.tab; if (['history', 'notes'].includes(detailsTab)) records.delete(detailsId); renderDetails(); };
  for (const tab of /** @type {NodeListOf<HTMLElement>} */ (document.querySelectorAll('.tab'))) tab.addEventListener('click', () => selectDetailsTab(tab));
  roveTabs(document.querySelector('.tabs'), selectDetailsTab);
  roveTabs(document.querySelector('.settings-nav'), tab => showSection(tab.dataset.section), true);
  // Sortable headers work with Enter and Space as well as clicks.
  els.playersBody.addEventListener('keydown', event => {
    const header = /** @type {HTMLElement | null} */ (/** @type {Element} */ (event.target).closest('th[data-sort]'));
    if (!header || !['Enter', ' '].includes(event.key)) return;
    event.preventDefault();
    header.click();
    requestAnimationFrame(() => els.playersBody.querySelector(`th[data-sort="${header.dataset.sort}"]`)?.focus());
  });
  document.addEventListener('keydown', event => { if (event.key === 'Escape' && detailsId && !document.activeElement?.classList.contains('hotkey')) { detailsId = ''; renderDetails(); } });
  // A click outside the drawer closes it; clicks in the player tables switch or act on players instead.
  document.addEventListener('click', event => {
    const target = event.target;
    if (!detailsId || !(target instanceof Element) || els.details.contains(target) || target.closest('.teams, .toast, .lobby-summary')) return;
    detailsId = '';
    renderDetails();
  });
  els.setupGuide.addEventListener('click', event => {
    const button = /** @type {HTMLButtonElement | null} */ (/** @type {Element} */ (event.target).closest('button[data-guide]'));
    if (!button) return;
    if (button.dataset.guide === 'settings') showView('settings');
    else if (button.dataset.guide === 'gsi') busy(button, async () => toast(`GSI config installed: ${(await api.installGsi()).file}. Restart CS2 if it is already running.`));
    else if (button.dataset.guide === 'launch') busy(button, async () => toast(await api.launchGame()));
  });
  bindSettings();
  api.onCommand(command => { if (command === 'open-setup') showView('settings'); });

  // Shortcut recorder: focus the field and press the combination.
  for (const [input, key] of [[els.overlayHotkey, 'overlayHotkey'], [els.interactHotkey, 'interactHotkey']]) {
    input.addEventListener('focus', () => input.classList.add('recording'));
    input.addEventListener('blur', () => input.classList.remove('recording'));
    input.addEventListener('keydown', event => {
      event.preventDefault();
      const accelerator = acceleratorFrom(event);
      if (accelerator) { input.value = accelerator; input.blur(); void save({ [key]: accelerator }); }
    });
  }

  els.overlayBtn.addEventListener('click', () => api.toggleOverlay());
  els.launchGameBtn.addEventListener('click', () => busy(els.launchGameBtn, async () => toast(await api.launchGame())));
  els.leetifyLink.addEventListener('click', () => api.openLink('leetify'));
  // Links to the project open in your browser (data-link names one of a fixed list in the app).
  document.addEventListener('click', event => {
    const link = /** @type {HTMLElement | null} */ (/** @type {Element} */ (event.target).closest('a[data-link]'));
    if (!link) return;
    event.preventDefault();
    api.openLink(link.dataset.link).catch(error => toast(error.message || String(error)));
  });
  els.openLogBtn.addEventListener('click', () => api.openDiagnosticsFolder());
  els.copyReportBtn.addEventListener('click', () => busy(els.copyReportBtn, copyReport));
  els.refreshBtn.addEventListener('click', () => busy(els.refreshBtn, async () => {
    toast('Refreshing player data…');
    await api.refresh();
    toast('Refresh complete. Values that failed to refresh keep their previous data, marked stale.');
  }));
  els.addBtn.addEventListener('click', () => busy(els.addBtn, async () => { await api.addSteamId(els.steamIdInput.value); els.steamIdInput.value = ''; toast('Player added.'); }));
  els.selectRosterBtn.addEventListener('click', () => busy(els.selectRosterBtn, async () => toast(`${(await api.selectPlayers(els.rosterInput.value)).count} players selected.`)));
  els.autoRosterBtn.addEventListener('click', () => busy(els.autoRosterBtn, async () => { await api.useAutomaticRoster(); toast('Automatic player list restored.'); }));
  els.removeKeyBtn.addEventListener('click', () => busy(els.removeKeyBtn, async () => { await api.saveSettings({ csrepApiKey: '' }); toast(els.csrepPagesEnabled.checked ? 'Saved CSRep key removed. Public profile pages are used instead.' : 'Saved CSRep key removed. CSRep is off until you add a key or allow its public pages.'); }));
  els.removeFaceitKeyBtn.addEventListener('click', () => busy(els.removeFaceitKeyBtn, async () => { await api.saveSettings({ faceitApiKey: '' }); toast('Saved FACEIT key removed. FACEIT level and Elo now come from Leetify.'); }));
  els.importHistoryBtn.addEventListener('click', () => busy(els.importHistoryBtn, async () => {
    const result = await api.importHistory();
    if (!result) return;
    records.clear();
    await loadDbStats();
    toast(`Imported ${result.matches} match${result.matches === 1 ? '' : 'es'}${result.skipped ? ` (${result.skipped} already here)` : ''} and ${result.notes} note${result.notes === 1 ? '' : 's'}.`);
  }));
  els.exportHistoryBtn.addEventListener('click', () => busy(els.exportHistoryBtn, async () => { const file = await api.exportHistory(); if (file) toast(`History exported to ${file}.`); }));
  els.clearHistoryBtn.addEventListener('click', () => busy(els.clearHistoryBtn, async () => {
    if (!confirm('Delete all recorded matches, encounters and measured values? Notes are kept. This cannot be undone.')) return;
    await api.clearHistory(false); records.clear(); await loadDbStats(); toast('Match history deleted.');
  }));
  els.clearAllBtn.addEventListener('click', () => busy(els.clearAllBtn, async () => {
    if (!confirm('Delete all recorded matches AND all your notes? This cannot be undone.')) return;
    await api.clearHistory(true); records.clear(); await loadDbStats(); toast('History and notes deleted.');
  }));
  els.playersBody.addEventListener('click', event => {
    const header = /** @type {HTMLElement | null} */ (/** @type {Element} */ (event.target).closest('th[data-sort]'));
    if (!header) return;
    const key = header.dataset.sort;
    // Highest first; clicking the same column again reverses it.
    sortDir = sortKey === key ? -sortDir : -1;
    sortKey = key;
    if (lastState) render(lastState);
  });
  els.removeSteamKeyBtn.addEventListener('click', () => busy(els.removeSteamKeyBtn, async () => { await api.saveSettings({ steamWebApiKey: '' }); toast('Saved Steam key removed. Hours show N/A without it.'); }));
  for (const button of /** @type {NodeListOf<HTMLButtonElement>} */ (document.querySelectorAll('.picker'))) {
    button.addEventListener('click', () => busy(button, async () => {
      const selected = await api.pickPath(button.dataset.kind);
      const input = /** @type {HTMLInputElement} */ (byId(button.dataset.target));
      if (selected) { input.value = selected; input.dispatchEvent(new Event('change')); }
    }));
  }
  els.installGsiBtn.addEventListener('click', () => busy(els.installGsiBtn, async () => toast(`GSI config installed: ${(await api.installGsi()).file}. Restart CS2 if it is already running.`)));
  els.csstatsLoginBtn.addEventListener('click', () => busy(els.csstatsLoginBtn, () => api.openCsStatsLogin()));
  els.csrepLoginBtn.addEventListener('click', () => busy(els.csrepLoginBtn, () => api.openCsRepLogin()));
  els.checkUpdatesBtn.addEventListener('click', () => busy(els.checkUpdatesBtn, async () => toast(updateText(await api.checkForUpdates()))));
  els.installUpdateBtn.addEventListener('click', () => busy(els.installUpdateBtn, () => api.installUpdate()));
}

decorate();
bind();
api.onState(render);
// "Updated … ago" ages without new state, so it is refreshed on a slow timer instead.
setInterval(updateFreshness, 15000);
