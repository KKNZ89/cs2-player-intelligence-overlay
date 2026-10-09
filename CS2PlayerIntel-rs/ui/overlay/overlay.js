import { byId, esc, setClass, setHidden, setHtml, setText } from '../lib/dom.js';
import { renderLiveStats } from '../lib/live-stats.js';
import { icon } from '../lib/icons.js';
import { api } from '../lib/bridge.js';
import { ago, avatar, colourClass, columns as c, groups, isNum, notesHtml, issuesHtml, mapRecord, matchTitle, na, playerChips, playerName, profileIndicator, rosterCounts, roundText, scoreHtml, setPriority, setPrivacy, statusIcons, teamAverages, winChance } from '../lib/format.js';

const card = document.querySelector('.overlay-card');
const list = byId('overlayPlayers');
let lastState = null;
let openId = '';
// A redraw between mouse down and up replaces the button, and the click is lost. While a button is held,
// new state waits and is drawn on release.
let pressed = false;
let heldState = null;
let pointerId = null;
let hintNote = '';
let hintTimer = 0;

// Width the side panel needs: the chosen columns plus at least NAME_WIDTH for names.
const NAME_WIDTH = 150;
let sideWidth = 500;
const fit = () => api.fitOverlay(Math.ceil(card.getBoundingClientRect().height), sideWidth).catch(error => console.error(error.message));
new ResizeObserver(fit).observe(card);

// Compact view: the essentials for every player.
/** @type {Array<[string, (player: any) => string]>} */
// Compact columns, chosen and ordered in Settings: label, width in the side panel, value.
const shortHours = p => (isNum(p.steam?.hoursCs2) ? `<span class="v" title="${p.steam.hoursCs2.toLocaleString('en-US')} CS2 hours (Steam)">${p.steam.hoursCs2 >= 1000 ? `${(p.steam.hoursCs2 / 1000).toFixed(1)}k` : p.steam.hoursCs2}</span>` : c.hours(p));
const COLUMNS = {
  hours: ['Hours', 40, shortHours], premier: ['Premier', 54, c.premier], faceit: ['FACEIT', 30, c.faceit], aim: ['Aim', 28, c.aim],
  ttd: ['TTD', 44, c.timeToDamage], hs: ['HS%', 32, c.hs], kd: ['K/D', 34, c.kd], win: ['Win', 34, c.winRate], csrep: ['CSRep', 38, c.csrep],
  profile: ['Profile', 50, p => (lastState?.settings?.profileIndicators === false ? na('Profile indicators are off') : profileIndicator(p, { short: true }))]
};
let COMPACT = [];

function chooseColumns(state) {
  const keys = (state.settings?.overlayColumns || ['hours', 'premier', 'faceit', 'aim', 'ttd', 'hs', 'profile']).filter(key => COLUMNS[key]);
  COMPACT = keys.map(key => [COLUMNS[key][0], COLUMNS[key][2]]);
  // Fixed widths per column keep every row of the side panel aligned (set through CSSOM: the CSP has no inline styles).
  list.style.setProperty('--ov-cols', `minmax(${NAME_WIDTH}px, 1fr) ${keys.map(key => `${COLUMNS[key][1]}px`).join(' ')}`);
  // Row padding (2 x 14), card border and padding (16), the name column, the columns and their 5 px gaps.
  const width = 28 + 16 + NAME_WIDTH + keys.reduce((sum, key) => sum + COLUMNS[key][1] + 5, 0);
  if (width !== sideWidth) {
    sideWidth = width;
    void fit();
  }
}

// Expanded view: column groups chosen in Settings.
const GROUPS = {
  performance: ['Performance', [['K/D', c.kd], ['Rating', c.rating], ['ADR', c.adr], ['HS %', c.hsKills], ['Win %', c.winRate], ['Last 10', c.last10], ['Matches', c.matches]]],
  reputation: ['Reputation', [['CSRep', c.csrep], ['VAC / game bans', c.vac]]],
  faceit: ['FACEIT', [['Level', c.faceit], ['Elo', c.faceitElo]]],
  map: ['Current map', [['Map record', null]]],
  leetify: ['Leetify', [['Aim', c.aim], ['Positioning', c.positioning], ['Utility', c.utility], ['Time to Damage', c.timeToDamage], ['Preaim', c.preaim]]],
  steam: ['Steam', [['Hours', c.hours], ['Account age', c.accountAge]]]
};

const ringClass = group => (group.ffa ? '' : group.side);
const nameCell = (p, group) => `<span class="ov-name"><span class="avatar-wrap ${ringClass(group)}${colourClass(p)}">${avatar(p, 20)}</span><span class="nm">${playerName(p)}</span>${statusIcons(p, { indicators: lastState?.settings?.profileIndicators !== false, history: false })}</span>`;

function teamHeader(group) {
  return `<header class="ov-team-head"><span class="ov-team-name" title="${esc(group.description || '')}">${group.title}</span><span class="ov-team-meta">${teamAverages(group.players)}</span></header>`;
}

// Side panel: a fixed column grid per team, so values line up like a scoreboard.
function sidePanel(state) {
  return groups(state).map(group => `
    <section class="ov-team ${group.side} ${group.teamSide ? `side-${group.teamSide}` : ''}">
      ${teamHeader(group)}
      <div class="ov-grid ov-cols"><span>Player</span>${COMPACT.map(([label]) => `<span>${label}</span>`).join('')}</div>
      ${group.players.map(p => `<div class="ov-grid ov-row${p.isSelf ? ' self' : ''}" data-player="${p.steamId}">${nameCell(p, group)}${COMPACT.map(([, render]) => `<span>${render(p)}</span>`).join('')}</div>`).join('')}
    </section>`).join('');
}

function table(state, columns, { heads = null, interactive = false } = {}) {
  const extra = interactive ? 1 : 0;
  const groupRow = heads ? `<tr class="ov-colgroups"><th></th>${heads.map(([title, span]) => `<th colspan="${span}">${title}</th>`).join('')}</tr>` : '';
  const header = `<tr><th>Player</th>${columns.map(([title]) => `<th>${title}</th>`).join('')}${interactive ? '<th></th>' : ''}</tr>`;
  const body = groups(state).map(group => `<tr class="ov-group ${group.side} ${group.teamSide ? `side-${group.teamSide}` : ''}"><td colspan="${columns.length + 1 + extra}" title="${esc(group.description || '')}">${group.title}<span class="ov-team-meta">${teamAverages(group.players)}</span></td></tr>${
    group.players.map(p => {
      const open = interactive && openId === p.steamId;
      const row = `<tr class="${group.side}${p.isSelf ? ' self' : ''}${interactive ? ' clickable' : ''}${open ? ' open' : ''}" data-player="${p.steamId}"${interactive ? ` data-action="toggle" data-id="${esc(p.steamId)}"` : ''}><td>${nameCell(p, group)}</td>${columns.map(([, render]) => `<td>${render(p)}</td>`).join('')}${interactive ? `<td class="ov-chevron">${icon('chevron', { size: 14 })}</td>` : ''}</tr>`;
      return open ? `${row}<tr class="ov-detail-row"><td colspan="${columns.length + 1 + extra}">${playerPanel(p, group)}</td></tr>` : row;
    }).join('')}`).join('');
  return `<table class="overlay-table${interactive ? ' interactive' : ''}"><thead>${groupRow}${header}</thead><tbody>${body}</tbody></table>`;
}

// Expanded player card in the Esc menu: the details that matter mid-match, plus actions.
function playerPanel(p, group) {
  const metric = (label, value) => `<div class="ov-metric"><span>${label}</span><b>${value}</b></div>`;
  const chips = playerChips(p, { inferred: group.inferred.has(p.steamId), state: lastState || {} });
  const id = esc(p.steamId);
  const sideButtons = p.isSelf || group.ffa ? '' : `<button class="ov-btn team${p.side === 'team' ? ' active' : ''}" data-action="side" data-side="${p.side === 'team' ? '' : 'team'}" data-id="${id}">Team</button><button class="ov-btn enemy${p.side === 'enemy' ? ' active' : ''}" data-action="side" data-side="${p.side === 'enemy' ? '' : 'enemy'}" data-id="${id}">Enemy</button>`;
  return `<div class="ov-panel">
    <div class="ov-panel-head"><div>${chips || `<span class="muted">${id}</span>`}${issuesHtml(p)}</div>
      <div class="ov-actions"><button class="ov-btn" data-action="refresh" data-id="${id}">${icon('refresh', { size: 13 })}<span>Refresh</span></button>${sideButtons}</div></div>
    <div class="ov-metrics">
      ${metric('Premier', c.premierFull(p))}${metric('Rating', c.rating(p))}${metric('K/D', c.kd(p))}${metric('ADR', c.adr(p))}${metric('Win rate', c.winRate(p))}${metric('HS %', c.hsKills(p))}
      ${metric('Time to Damage', c.timeToDamage(p))}${metric('Head Accuracy', c.headAccuracy(p))}${metric('Aim', c.aim(p))}${metric('Positioning', c.positioning(p))}${metric('Utility', c.utility(p))}${metric('FACEIT', c.faceit(p))}
      ${metric('CSRep Trust', c.csrep(p))}${metric('CSRep verdict', p.csrep?.trustVerdict ? esc(p.csrep.trustVerdict) : '<span class="na">N/A</span>')}${metric('VAC / game bans', c.vac(p))}${metric('Hours', c.hours(p))}${metric('Account age', c.accountAge(p))}
    </div>
    <div class="ov-notes">${notesHtml(p.notes, { limit: 3 })}
      <div class="ov-note-add"><input class="ov-note-input" data-id="${id}" maxlength="2000" placeholder="${lastState?.overlayMouseNavigation ? 'Add a note about this player' : 'Add a note: press Shift+F8 to type'}"><button class="ov-btn" data-action="note" data-id="${id}">Save note</button></div></div>
    <div class="ov-panel-foot"><span class="ov-form">Last 10 ${c.last10(p)}</span>
      <span class="ov-links">${['leetify', 'csrep', 'csstats', 'steam'].map(provider => `<button class="ov-link" data-action="profile" data-provider="${provider}" data-id="${id}">${{ leetify: 'Leetify', csrep: 'CSRep', csstats: 'CSStats', steam: 'Steam' }[provider]}${icon('external', { size: 11 })}</button>`).join('')}</span></div>
  </div>`;
}

function expanded(state) {
  const map = state.gsi?.map || '';
  const columns = [];
  const heads = [];
  for (const name of (state.settings?.expandedGroups || ['performance']).filter(group => GROUPS[group])) {
    const [title, entries] = GROUPS[name];
    const resolved = entries.map(([label, render]) => [label, render || (p => mapRecord(p, map))]);
    columns.push(...resolved);
    heads.push([name === 'map' && map ? `${title}: ${map.replace(/^(de|cs)_/, '')}` : title, resolved.length]);
  }
  return table(state, columns, { heads });
}

function renderPlayers(state, view, layout) {
  if (!(state.players || []).length) return '<p class="ov-empty">Players appear automatically once a match is running.</p>';
  // The overlay is for the match in progress; finished matches are on the History page in the app.
  if (state.lastMatch) return '<p class="ov-empty">The match is over. Its players are on the History page in the app.</p>';
  if (view === 'interactive') return table(state, COMPACT, { interactive: true });
  if (view === 'expanded') return expanded(state);
  return layout === 'top' ? table(state, COMPACT) : sidePanel(state);
}

function updateCount() {
  const count = (lastState?.players || []).length;
  const { confirmed, possible, max } = rosterCounts(lastState || {});
  setText(byId('overlayCount'), `${Math.min(count, max)}/${max} listed (${confirmed} confirmed, ${possible} possible)${count > max ? ` · ${max} of ${count} shown` : ''} · updated ${ago(lastState?.coverage?.lastFetchedAt)}`);
}

function interactionHint() {
  if (lastState?.overlayMouseNavigation) {
    return 'Mouse navigation: click a player for profiles. Press Esc or the overlay shortcut again to exit; click back into CS2 to resume.';
  }
  return lastState?.gsi?.activity === 'playing'
    ? 'Press Esc in CS2 to release the mouse, then click a player for details. The overlay shortcut does not release the game cursor.'
    : 'Esc menu: click a player for details · press Esc again to return to the game';
}

function render(state) {
  if (pressed && state.overlayView === 'interactive') { heldState = state; return; }
  pressed = false;
  pointerId = null;
  heldState = null;
  lastState = state;
  setPrivacy(state.settings?.privacyMode === true, state.players || []);
  setPriority(state.settings?.sourcePriority);
  chooseColumns(state);
  const g = state.gsi || {};
  const match = state.match || { state: 'offline', label: 'Waiting' };
  const view = ['expanded', 'interactive'].includes(state.overlayView) ? state.overlayView : 'compact';
  const layout = view === 'compact' ? state.settings?.overlayLayout || 'right' : 'top';
  if (view !== 'interactive') openId = '';
  setClass(document.body, `overlay-body layout-${layout} view-${view} style-${state.settings?.overlayStyle === 'minimal' ? 'minimal' : 'standard'}`);
  card.style.opacity = String(view === 'interactive' ? 1 : state.settings?.overlayOpacity ?? 0.94);
  setClass(byId('overlayPill'), `pill ${match.state}`);
  setText(byId('overlayState'), match.label);
  setText(byId('overlayMatch'), matchTitle(g));
  setHtml(byId('ovScore'), scoreHtml(g));
  setText(byId('ovRound'), roundText(g));
  const win = winChance(state.prediction);
  setText(byId('ovWinValue'), win.text);
  byId('ovWinValue').title = win.detail;
  setClass(byId('ovWinValue'), win.percent === null ? '' : win.percent >= 55 ? 'good' : win.percent <= 45 ? 'bad' : 'even');
  byId('ovWinBar').style.width = `${win.percent ?? 50}%`;
  setClass(byId('ovWinBar').parentElement, `win-bar thin side-${match.side || 'none'}${win.percent === null ? ' unknown' : ''}`);
  setHidden(byId('ovHint'), view !== 'interactive');
  setText(byId('ovHint'), hintNote || interactionHint());
  renderLiveStats(byId('overlayLiveStats'), g);
  updateCount();
  // A note being typed survives redraws (new data arrives while you type).
  const draft = /** @type {HTMLInputElement | null} */ (list.querySelector('.ov-note-input'));
  const typing = draft ? { id: draft.dataset.id, value: draft.value, focused: document.activeElement === draft, caret: draft.selectionStart } : null;
  setHtml(list, renderPlayers(state, view, layout));
  const input = typing && /** @type {HTMLInputElement | null} */ (list.querySelector(`.ov-note-input[data-id="${typing.id}"]`));
  if (input && input.value !== typing.value) {
    input.value = typing.value;
    if (typing.focused) {
      input.focus();
      input.setSelectionRange(typing.caret, typing.caret);
    }
  }
}

function note(text) {
  hintNote = text;
  setText(byId('ovHint'), text || interactionHint());
  clearTimeout(hintTimer);
  if (text) hintTimer = setTimeout(() => note(''), 6000);
}

list.addEventListener('pointerdown', event => {
  if (lastState?.overlayView !== 'interactive' || event.button !== 0 || !event.isPrimary || pressed) return;
  // Capture on the original target so releases outside the window still arrive without retargeting clicks.
  if (!(event.target instanceof Element)) return;
  event.target.setPointerCapture(event.pointerId);
  pressed = true;
  pointerId = event.pointerId;
});
const release = event => {
  if (event.type !== 'blur' && event.pointerId !== pointerId) return;
  if (!pressed) return;
  const releasedId = pointerId;
  // After the click event has fired.
  setTimeout(() => {
    if (pointerId !== releasedId) return;
    pressed = false;
    pointerId = null;
    if (heldState) { const state = heldState; heldState = null; render(state); }
  }, 0);
};
window.addEventListener('pointerup', release);
window.addEventListener('pointercancel', release);
window.addEventListener('blur', release);
list.addEventListener('lostpointercapture', release);

window.addEventListener('keydown', event => {
  if (event.key !== 'Escape' || !lastState?.overlayMouseNavigation) return;
  event.preventDefault();
  api.endOverlayInteraction().catch(error => { console.error(error.message); note(error.message); });
});

// Clicks only reach the overlay while CS2's Esc menu is open (it is click-through otherwise).
list.addEventListener('click', async event => {
  const target = /** @type {HTMLElement | null} */ (/** @type {HTMLElement} */ (event.target).closest('[data-action]'));
  if (!target) return;
  const { action, id } = target.dataset;
  if (action === 'toggle') { openId = openId === id ? '' : id; if (lastState) render(heldState || lastState); return; }
  event.stopPropagation();
  if (target instanceof HTMLButtonElement) target.disabled = true;
  try {
    if (action === 'refresh') { target.classList.add('spin'); await api.refreshPlayer(id); }
    else if (action === 'side') await api.setPlayerSide(id, target.dataset.side);
    else if (action === 'note') {
      const input = /** @type {HTMLInputElement | null} */ (target.closest('.ov-note-add')?.querySelector('.ov-note-input'));
      if (input?.value.trim()) {
        await api.addNote(id, input.value);
        input.value = '';
        note('Note saved to this match.');
      } else note('Type the note first (press Shift+F8 so the overlay can take keyboard input).');
    }
    else if (action === 'profile') {
      note(`Opening ${target.textContent.trim()}… it shows over the game; click back into CS2 to hide it.`);
      await api.openProfile(target.dataset.provider, id);
    }
  } catch (error) { console.error(error.message); note(error.message); }
  finally { if (target instanceof HTMLButtonElement) target.disabled = false; target.classList.remove('spin'); }
});

list.addEventListener('keydown', event => {
  const input = /** @type {Element} */ (event.target).closest('.ov-note-input');
  if (input && event.key === 'Enter') /** @type {HTMLElement} */ (input.parentElement.querySelector('[data-action="note"]')).click();
});

api.onState(render);
setInterval(updateCount, 15000);
