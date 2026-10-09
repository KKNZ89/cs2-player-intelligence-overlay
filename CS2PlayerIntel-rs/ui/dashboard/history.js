// History page: your recorded matches, and who was in each one with their values at the time.
import { byId, esc, setHidden, setHtml, setText } from '../lib/dom.js';
import { icon } from '../lib/icons.js';
import { api } from '../lib/bridge.js';
import { displayName, matchTitle, notesHtml } from '../lib/format.js';

const PAGE = 50;
const OUTCOME = { win: ['W', 'Win'], loss: ['L', 'Loss'], tie: ['T', 'Tie'] };
const SIDES = [['team', 'Your team'], ['enemy', 'Opponents'], ['', 'Not assigned']];

let matches = [];
let total = 0;
let openId = null;
let onError = message => console.error(message);

// Short dates in the list ("8 Oct, 08:12"); the year only when it isn't this year.
const when = time => {
  const date = new Date(time);
  const year = date.getFullYear() === new Date().getFullYear() ? undefined : 'numeric';
  return date.toLocaleString([], { day: 'numeric', month: 'short', year, hour: '2-digit', minute: '2-digit' });
};
const whenFull = time => new Date(time).toLocaleString([], { dateStyle: 'full', timeStyle: 'short' });
const title = m => matchTitle({ connected: true, mode: m.mode, map: m.map }).replace('In menus', 'Unknown map');
const outcome = result => {
  const [short, long] = OUTCOME[result] || ['?', 'Result unknown'];
  return `<span class="match-chip small ${OUTCOME[result] ? result : 'unknown'}" title="${long}">${short}</span>`;
};

// A value recorded at the end of the match, or a dash.
const value = (values, key, format = v => v.toLocaleString('en-US')) => (Number.isFinite(values?.[key]) ? `<span class="v">${format(values[key])}</span>` : '<span class="muted">—</span>');
const percent = v => `${Math.round(v)}%`;
const fixed = digits => v => v.toFixed(digits);

function renderList() {
  setText(byId('historyCount'), total ? `${total} match${total === 1 ? '' : 'es'} recorded` : '');
  setHidden(byId('historyEmpty'), total > 0);
  setHidden(byId('historyMoreBtn'), matches.length >= total);
  const count = m => [`${m.team} teammate${m.team === 1 ? '' : 's'}`, `${m.enemy} opponent${m.enemy === 1 ? '' : 's'}`, m.players > m.team + m.enemy ? `${m.players - m.team - m.enemy} not assigned` : ''].filter(Boolean).join(' · ');
  setHtml(byId('historyList'), matches.map(m => `<tr class="clickable${m.id === openId ? ' open' : ''}" data-match="${m.id}" tabindex="0" title="${esc(count(m))} recorded">
      <td>${esc(when(m.endedAt))}</td><td>${esc(title(m))}</td><td>${outcome(m.result)}</td></tr>`).join(''));
}

function playerRows(players) {
  return players.map(p => {
    const name = displayName({ steamId: p.steamId, name: p.name }) || p.steamId;
    const note = p.hasNote ? `<span class="history-note" title="You have a note about this player">${icon('note', { size: 13 })}</span>` : '';
    return `<tr>
      <td><span class="history-name">${esc(name)}</span>${note}</td>
      <td class="num" title="Matches with this player you have recorded">${p.met}×</td>
      <td class="num">${value(p.values, 'steam.hoursCs2')}</td>
      <td class="num">${value(p.values, 'csrep.trust', percent)}</td>
      <td class="num">${value(p.values, 'faceit.level')}</td>
      <td class="num">${value(p.values, 'faceit.elo')}</td>
      <td class="num">${value(p.values, 'csstats.kd', fixed(2))}</td>
      <td class="num">${value(p.values, 'csstats.hs', percent)}</td>
      <td class="history-links"><button class="link" type="button" data-note-toggle="${esc(p.steamId)}">Note</button><button class="link" type="button" data-profile="steam" data-id="${esc(p.steamId)}">Steam</button><button class="link" type="button" data-profile="leetify" data-id="${esc(p.steamId)}">Leetify</button></td>
    </tr>
    ${p.notes?.length ? `<tr class="history-notes"><td colspan="9">${notesHtml(p.notes)}</td></tr>` : ''}
    <tr class="history-note-add" data-note-row="${esc(p.steamId)}" hidden><td colspan="9"><div class="input-row"><input class="input" maxlength="2000" placeholder="Note about ${esc(name)} in this match"><button class="btn" type="button" data-note-save="${esc(p.steamId)}">Save note</button></div></td></tr>`;
  }).join('');
}

function renderMatch(m) {
  setHidden(byId('historyMatch'), !m);
  if (!m) return;
  setText(byId('historyMatchTitle'), `${title(m)} · ${{ win: 'Win', loss: 'Loss', tie: 'Tie' }[m.result] || 'Result unknown'}`);
  setText(byId('historyMatchWhen'), whenFull(m.endedAt));
  const groups = SIDES.map(([side, label]) => [label, m.players.filter(p => (p.side || '') === side)]).filter(([, players]) => players.length);
  setHtml(byId('historyMatchPlayers'), groups.length
    ? groups.map(([label, players]) => `<h3 class="sub-head">${label}</h3>
      <div class="table-wrap"><table class="table history-players"><thead><tr><th>Player</th><th class="num">Met</th><th class="num">Hours</th><th class="num">CSRep</th><th class="num">FACEIT</th><th class="num">Elo</th><th class="num">K/D</th><th class="num">HS %</th><th></th></tr></thead>
      <tbody>${playerRows(players)}</tbody></table></div>`).join('')
    : '<p class="muted">No other players were recorded for this match.</p>');
}

async function loadMore() {
  const page = await api.historyMatches(PAGE, matches.length);
  total = page.total;
  matches = matches.concat(page.matches);
  renderList();
}

async function open(id) {
  openId = id;
  renderList();
  try {
    renderMatch(await api.historyMatch(id));
  } catch (error) {
    renderMatch(null);
    onError(error.message || String(error));
  }
}

async function saveNote(button) {
  const input = /** @type {HTMLInputElement} */ (button.closest('tr').querySelector('input'));
  if (!input.value.trim() || openId === null) return;
  try {
    await api.addNote(button.dataset.noteSave, input.value, openId);
    renderMatch(await api.historyMatch(openId));
  } catch (error) {
    onError(error.message || String(error));
  }
}

/** Loads the first page again (each time the page is shown, so new matches appear). */
export async function loadHistory() {
  matches = [];
  try {
    await loadMore();
    if (openId !== null && !matches.some(m => m.id === openId)) {
      openId = null;
      renderMatch(null);
    }
  } catch (error) {
    onError(error.message || String(error));
  }
}

export function bindHistory(reportError) {
  onError = reportError;
  const list = byId('historyList');
  const pick = event => {
    const row = /** @type {HTMLElement | null} */ (/** @type {Element} */ (event.target).closest('tr[data-match]'));
    if (row) void open(Number(row.dataset.match));
  };
  list.addEventListener('click', pick);
  list.addEventListener('keydown', event => {
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      pick(event);
    }
  });
  byId('historyMoreBtn').addEventListener('click', () => loadMore().catch(error => onError(error.message || String(error))));
  const players = byId('historyMatchPlayers');
  players.addEventListener('click', event => {
    const target = /** @type {Element} */ (event.target);
    const profile = /** @type {HTMLElement | null} */ (target.closest('button[data-profile]'));
    if (profile) api.openProfile(profile.dataset.profile, profile.dataset.id).catch(error => onError(error.message || String(error)));
    const toggle = /** @type {HTMLElement | null} */ (target.closest('button[data-note-toggle]'));
    if (toggle) {
      const row = /** @type {HTMLElement | null} */ (players.querySelector(`tr[data-note-row="${CSS.escape(toggle.dataset.noteToggle)}"]`));
      if (row) {
        row.hidden = !row.hidden;
        if (!row.hidden) /** @type {HTMLInputElement} */ (row.querySelector('input')).focus();
      }
    }
    const save = /** @type {HTMLElement | null} */ (target.closest('button[data-note-save]'));
    if (save) void saveNote(save);
  });
  players.addEventListener('keydown', event => {
    const input = /** @type {Element} */ (event.target).closest('tr[data-note-row] input');
    if (input && event.key === 'Enter') void saveNote(/** @type {HTMLElement} */ (input.closest('tr').querySelector('button[data-note-save]')));
  });
}
