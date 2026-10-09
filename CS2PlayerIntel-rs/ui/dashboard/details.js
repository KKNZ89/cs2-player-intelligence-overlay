// Player details drawer: Overview, Performance, Matches, History and Notes. Every value names its source;
// measured values, inferred indicators and your own notes stay visibly separate.
import { esc } from '../lib/dom.js';
import { providerStatusText } from '../lib/status.js';
import { columns as c, avatar, displayName, history, isNum, mapRecord, na, playerName, premierPeak, profileIndicator, shownId, statusIcons } from '../lib/format.js';

const metric = (label, value, note = '') => `<div class="metric"><span>${label}</span><b>${value}</b>${note ? `<small>${note}</small>` : ''}</div>`;
const section = (title, content, aside = '') => `<section class="detail-section"><h3>${title}${aside ? `<span class="h3-aside">${aside}</span>` : ''}</h3>${content}</section>`;
const link = (provider, label, id) => `<button class="link" data-action="profile" data-provider="${provider}" data-id="${esc(id)}">${label}</button>`;
const OUTCOME = { win: 'W', loss: 'L', tie: 'T' };
const SOURCE = { matchmaking: 'Matchmaking', faceit: 'FACEIT', hltv: 'HLTV', esplay: 'Esplay', renown: 'Renown' };
const date = time => (time ? new Date(time).toLocaleDateString() : '—');
const num = (value, digits = 0) => (isNum(value) ? value.toLocaleString('en-US', { maximumFractionDigits: digits, minimumFractionDigits: digits }) : null);

function lastUpdated(p) {
  const times = ['leetify', 'csrep', 'csstats', 'steam', 'faceit'].map(name => p[name]?.fetchedAt).filter(Boolean);
  return times.length ? new Date(Math.max(...times)).toLocaleTimeString() : 'not yet';
}

function identity(p) {
  const side = p.isSelf ? 'You' : p.side === 'team' ? 'Your team' : p.side === 'enemy' ? 'Opponent' : 'Side not assigned';
  const id = shownId(p);
  return `<div class="identity">${avatar(p, 48)}<div>
    <div class="identity-name">${playerName(p)}${statusIcons(p)}</div>
    <div class="muted small">${id ? `${esc(id)} · ` : ''}${side} · updated ${lastUpdated(p)}</div>
    <div class="identity-ranks"><span>Premier ${c.premierFull(p)}</span><span>FACEIT ${c.faceit(p)}</span><span>Profile ${profileIndicator(p)}</span></div>
  </div></div>`;
}

// ---- Overview ---------------------------------------------------------------------------------------

function profileSummary(p) {
  const a = p.analysis;
  if (p.isSelf) return '<p class="muted">This is you.</p>';
  if (!a) return '<p class="muted">Analysed once provider data arrives.</p>';
  const heading = { review: 'Profile review', normal: 'No notable discrepancies', insufficient: 'Not enough data to compare' }[a.indicator];
  const reasons = a.reasons.length ? `<ul class="reasons">${a.reasons.map(r => `<li>${esc(r)}</li>`).join('')}</ul>` : '';
  const single = a.indicator === 'normal' && a.reasons.length === 1 ? '<p class="muted small">One discrepancy on its own is common and not flagged.</p>' : '';
  return `<div class="summary ${a.indicator}">
    <div class="summary-head">${profileIndicator(p)}<b>${heading}</b>${a.indicator === 'insufficient' ? '' : `<span class="confidence">${a.confidence} confidence</span>`}</div>
    ${reasons}${single}
    <p class="muted small">Compared: ${a.checked.length ? esc(a.checked.join(', ')) : 'nothing yet (needs at least two of Premier, FACEIT, CS2 hours, account age and Leetify aim)'}.
    Confidence reflects how many comparisons were possible and how fresh the data is. Thresholds are fixed rules of thumb, not reference-group percentiles. A statistical discrepancy is not evidence of cheating.</p>
  </div>`;
}

function overview(p) {
  const tendencies = p.analysis?.tendencies || [];
  return `<div class="detail-columns">
    <div>${section('Key numbers', `<div class="metrics two">
      ${metric('CS2 hours', c.hours(p))}${metric('Account age', c.accountAge(p))}
      ${metric('Premier', c.premierFull(p))}${metric('Premier peak', c.premierPeak(p), 'recent matches')}
      ${metric('FACEIT level', c.faceit(p))}${metric('FACEIT Elo', c.faceitElo(p))}
      ${metric('FACEIT matches', c.faceitMatches(p))}${metric('Leetify aim', c.aim(p))}
      ${metric('Time to Damage', c.timeToDamage(p))}${metric('HS %', c.hs(p), 'headshot kills')}
    </div>`)}</div>
    <div>${section('Profile summary', profileSummary(p))}
      ${section('Historical tendencies', tendencies.length
        ? `<ul class="reasons">${tendencies.map(t => `<li>${esc(t)}</li>`).join('')}</ul><p class="muted small">From Leetify's aggregates over past matches; not a prediction of this match.</p>`
        : `<p class="muted">${p.leetify?.status === 'ok' ? 'No clear tendencies, or too few Leetify matches (see Settings → Analysis).' : 'Needs a Leetify profile.'}</p>`)}
      ${section('Played before', `<p>${history(p)}</p>`)}</div>
  </div>
  <div class="profile-links">${link('steam', 'Steam', p.steamId)}${link('faceit', 'FACEIT', p.steamId)}${link('leetify', 'Leetify', p.steamId)}${link('csrep', 'CSRep', p.steamId)}${link('csstats', 'CSStats', p.steamId)}</div>`;
}

// ---- Performance ------------------------------------------------------------------------------------

function csrepSection(p) {
  const r = p.csrep;
  if (!r || r.status !== 'ok') return `<p>${na(providerStatusText(r || { status: 'pending' }))} No CSRep result. That says nothing about the player either way.</p>`;
  const rows = (list, render) => (list.length ? `<table class="detail-table">${list.map(render).join('')}</table>` : '<p class="muted">Not shown to signed-out visitors.</p>');
  return `<div class="metrics">${metric('Trust Score', c.csrep(p))}${metric('CSRep verdict', r.trustVerdict ? esc(r.trustVerdict) : na('Not published'))}</div>
    <h4>Breakdown</h4>${rows(r.breakdown || [], row => `<tr><td>${esc(row.label)}</td><td>${esc(row.value)}</td></tr>`)}
    <h4>Anomalies</h4>${rows(r.anomalies || [], row => `<tr><td>${esc(row.label)}</td><td>${esc(row.verdict)}</td></tr>`)}
    <p class="muted small">CSRep's own labels and verdicts, read from its public page (not a cheating verdict; a missing result is not a clean record) · fetched ${r.fetchedAt ? new Date(r.fetchedAt).toLocaleString() : 'unknown'}.</p>`;
}

function performance(p, state) {
  const l = p.leetify || {};
  const pct = (key, label) => metric(label, isNum(l[key]) ? `<span class="v" title="Leetify">${num(l[key])}%</span>` : na(l.status === 'ok' ? 'Leetify: not published' : 'Leetify: not loaded'));
  const rating = (key, label) => metric(label, isNum(l[key]) ? `<span class="v" title="Leetify rating (Leetify's own scale)">${l[key] > 0 ? '+' : ''}${l[key].toFixed(3)}</span>` : na('Leetify: not published'));
  const map = state.gsi?.map || '';
  const mapRank = (l.mapRanks || []).find(r => r.map === map);
  return [
    section('Results', `<div class="metrics">${metric('K/D', c.kd(p))}${metric('ADR', c.adr(p))}${metric('Rating', c.rating(p))}${metric('HS %', c.hs(p))}${metric('Win rate', c.winRate(p))}${metric('Matches', c.matches(p))}</div>
      <p class="muted small">K/D, ADR and HS % come from CSStats, CSRep or FACEIT when available. Leetify's public data has no K/D, ADR, KAST or opening-kill counts.</p>`),
    section('Mechanics', `<div class="metrics">${metric('Aim', c.aim(p))}${metric('Time to Damage', c.timeToDamage(p))}${metric('Preaim', c.preaim(p))}${metric('Head Accuracy', c.headAccuracy(p))}${pct('sprayAccuracy', 'Spray accuracy')}${pct('counterStrafing', 'Counter-strafing')}</div>`, 'Leetify'),
    section('Playstyle', `<div class="metrics">${pct('ctOpeningDuelSuccess', 'CT opening duels won')}${pct('tOpeningDuelSuccess', 'T opening duels won')}${pct('tradeKillsSuccess', 'Trade kills')}${rating('opening', 'Opening rating')}${rating('clutch', 'Clutch rating')}${rating('ctLeetify', 'CT rating')}${rating('tLeetify', 'T rating')}${metric('Positioning', c.positioning(p))}${metric('Utility', c.utility(p))}</div>
      <p class="muted small">Historical tendencies from past matches, not predictions of positions in this one. AWP usage is not published by any connected source.</p>`, 'Leetify'),
    section(`This map${map ? ` · ${esc(map.replace(/^(de|cs)_/, ''))}` : ''}`, `<div class="metrics">${metric('Recent record', mapRecord(p, map))}${metric('Competitive rank here', mapRank && mapRank.rank > 0 ? `<span class="v" title="Leetify: Competitive skill group on this map">${mapRank.rank} / 18</span>` : na(map ? 'Unranked here or not published' : 'No current map'))}</div>`),
    section('CSRep', csrepSection(p))
  ].join('');
}

// ---- Matches ----------------------------------------------------------------------------------------

// Premier rating in their recent Leetify Premier matches: one thin line, oldest to newest.
function premierChart(p) {
  const points = (p.leetify?.recent || []).filter(m => m.rankType === 11 && isNum(m.rank) && m.rank > 0 && m.finishedAt).map(m => ({ t: Date.parse(m.finishedAt), r: m.rank, map: m.map, outcome: m.outcome })).filter(m => isNum(m.t)).sort((a, b) => a.t - b.t);
  const total = (p.leetify?.recent || []).length;
  if (points.length < 2) return `<p class="muted">${points.length ? 'Only one recent Premier match on Leetify.' : 'No recent Premier matches on Leetify.'}</p>`;
  const W = 560, H = 150, L = 48, R = 56, T = 12, B = 22;
  const min = Math.min(...points.map(d => d.r)), max = Math.max(...points.map(d => d.r));
  const step = Math.max(500, Math.ceil((max - min) / 3 / 500) * 500);
  const lo = Math.floor(min / step) * step, hi = Math.max(lo + step, Math.ceil(max / step) * step);
  const x = i => L + (i / (points.length - 1)) * (W - L - R);
  const y = r => T + (1 - (r - lo) / (hi - lo)) * (H - T - B);
  const ticks = [];
  for (let v = lo; v <= hi; v += step) ticks.push(v);
  const grid = ticks.map(v => `<line class="grid" x1="${L}" x2="${W - R}" y1="${y(v)}" y2="${y(v)}"/><text class="tick" x="${L - 6}" y="${y(v) + 4}" text-anchor="end">${v.toLocaleString('en-US')}</text>`).join('');
  const path = points.map((d, i) => `${i ? 'L' : 'M'}${x(i).toFixed(1)},${y(d.r).toFixed(1)}`).join('');
  const hits = points.map((d, i) => `<g class="pt"><circle class="hit" cx="${x(i)}" cy="${y(d.r)}" r="9"/><circle class="dot" cx="${x(i)}" cy="${y(d.r)}" r="4"/><title>${esc(`${d.r.toLocaleString('en-US')} · ${d.map || 'unknown map'} · ${OUTCOME[d.outcome] || '?'} · ${new Date(d.t).toLocaleDateString()}`)}</title></g>`).join('');
  const last = points[points.length - 1];
  return `<figure class="chart">
    <figcaption><b>Premier rating per match</b><span class="muted small">${date(points[0].t)} – ${date(last.t)} · ${points.length} of their last ${total} Leetify matches had a Premier rating</span></figcaption>
    <svg viewBox="0 0 ${W} ${H}" role="img" aria-label="Premier rating from ${points[0].r} to ${last.r}">${grid}<path class="line" d="${path}"/>${hits}
      <text class="end-label" x="${x(points.length - 1) + 8}" y="${y(last.r) + 4}">${last.r.toLocaleString('en-US')}</text></svg>
  </figure>`;
}

function matches(p) {
  const recent = p.leetify?.recent;
  if (!Array.isArray(recent) || !recent.length) return `<p class="muted">${p.leetify?.status === 'ok' ? 'No recent matches on Leetify.' : 'Recent matches need a Leetify profile.'}</p>`;
  const rank = m => (m.rankType === 11 ? `Premier ${num(m.rank)}` : m.dataSource === 'faceit' && isNum(m.rank) ? `FACEIT ${m.rank}` : m.rankType === 12 && isNum(m.rank) ? `Skill group ${m.rank}` : '—');
  const rows = recent.slice(0, 30).map(m => `<tr><td>${date(m.finishedAt)}</td><td>${esc(SOURCE[m.dataSource] || m.dataSource || '—')}</td><td>${esc((m.map || '').replace(/^(de|cs)_/, ''))}</td>
    <td><span class="match-chip small ${m.outcome}">${OUTCOME[m.outcome] || '?'}</span> ${Array.isArray(m.score) ? m.score.join(':') : ''}</td><td>${rank(m)}</td><td class="num">${isNum(m.leetifyRating) ? m.leetifyRating.toFixed(2) : '—'}</td></tr>`).join('');
  const faceit = p.faceit?.recentResults;
  return premierChart(p)
    + section('Recent matches', `<table class="detail-table matches-table"><thead><tr><th>Date</th><th>Source</th><th>Map</th><th>Result</th><th>Rank then</th><th class="num">Leetify rating</th></tr></thead><tbody>${rows}</tbody></table>
      <p class="muted small">From Leetify, newest first, shown live and never stored.</p>`, `${Math.min(30, recent.length)} of ${recent.length}`)
    + (Array.isArray(faceit) && faceit.length ? section('FACEIT recent results', `<div class="match-strip">${faceit.map(r => `<span class="match-chip ${r}">${OUTCOME[r]}</span>`).join('')}</div>`, 'FACEIT API') : '');
}

// ---- History (this app's records) -------------------------------------------------------------------

const METRIC_LABELS = { hoursCs2: ['CS2 hours', v => num(v)], createdAt: ['Account created', v => date(v)], vacBans: ['VAC bans', v => num(v)], gameBans: ['Game bans', v => num(v)], trust: ['CSRep Trust Score', v => `${num(v)}%`], kd: ['K/D', v => num(v, 2)], hs: ['HS %', v => `${num(v)}%`], winRate: ['Win rate', v => `${num(v)}%`], elo: ['FACEIT Elo', v => num(v)], level: ['FACEIT level', v => num(v)], matches: ['FACEIT matches', v => num(v)] };
const PROVIDER_LABELS = { steam: 'Steam', csrep: 'CSRep', csstats: 'CSStats', faceit: 'FACEIT' };

function historyTab(p, record) {
  if (p.isSelf) return '<p class="muted">Your own matches are not tracked as encounters.</p>';
  if (!record) return '<p class="muted">Loading this app\'s records…</p>';
  const s = record.summary || {};
  const all = s.all || { played: 0 };
  if (!all.played && !(record.progression || []).length) return '<p class="muted">Not met in any match recorded by this app. Records start from matches played with the app running.</p>';
  const tally = t => `${t.played}× · ${t.won}–${t.lost}${t.tied ? `–${t.tied}` : ''}`;
  const encounters = (record.encounters || []).map(e => `<tr><td>${new Date(e.endedAt).toLocaleString()}</td><td>${esc((e.map || '').replace(/^(de|cs)_/, ''))}</td><td>${esc(e.mode || '')}</td><td>${{ team: 'Teammate', enemy: 'Opponent' }[e.side] || '—'}</td><td><span class="match-chip small ${e.result}">${OUTCOME[e.result] || '?'}</span></td></tr>`).join('');
  const change = (first, current, fmt) => {
    if (!first || !current || first.measuredAt === current.measuredAt || !isNum(first.value) || !isNum(current.value)) return '—';
    const d = current.value - first.value;
    return d === 0 ? 'no change' : `${d > 0 ? '+' : ''}${fmt === date ? Math.round(d / 864e5) + ' days' : num(d, Number.isInteger(d) ? 0 : 2)}`;
  };
  const cell = (point, fmt) => (point ? `<span title="Measured ${new Date(point.measuredAt).toLocaleString()}">${fmt(point.value)}</span>` : '—');
  const progression = (record.progression || []).map(row => {
    const [label, fmt] = METRIC_LABELS[row.metric] || [row.metric, v => num(v, 2)];
    return `<tr><td>${label} <span class="muted small">${PROVIDER_LABELS[row.provider] || row.provider}</span></td><td>${cell(row.first, fmt)}</td><td>${cell(row.previous, fmt)}</td><td>${cell(row.current, fmt)}</td><td>${change(row.first, row.current, fmt)}</td></tr>`;
  }).join('');
  return section('Encounters', `<div class="metrics">${metric('Matches together or against', tally(all))}${metric('As teammates', s.together?.played ? tally(s.together) : '—')}${metric('As opponents', s.against?.played ? tally(s.against) : '—')}${metric('First met', date(s.firstPlayedAt))}${metric('Last met', date(s.lastPlayedAt))}</div>
      ${encounters ? `<table class="detail-table"><thead><tr><th>Ended</th><th>Map</th><th>Mode</th><th>Side</th><th>Your result</th></tr></thead><tbody>${encounters}</tbody></table>` : ''}
      <p class="muted small">Recorded by this app on this PC. Their in-match stats are not recorded: CS2 only reports your own.</p>`)
    + section('Measured over time', progression
      ? `<table class="detail-table"><thead><tr><th>Value</th><th>First seen</th><th>Previous</th><th>Latest</th><th>Change</th></tr></thead><tbody>${progression}</tbody></table>
        <p class="muted small">Each value is kept as the source reported it, with that source's time (hover). Leetify values are not stored, by Leetify's rules; their history is under Matches.</p>`
      : '<p class="muted">No values recorded yet. They are saved from Steam, CSRep, CSStats and FACEIT when a match ends.</p>');
}

function notesTab(p, record) {
  if (!record) return '<p class="muted">Loading…</p>';
  const note = record.note;
  return section('Your note', `<textarea id="noteText" class="input note-input" rows="8" maxlength="2000" spellcheck="true" placeholder="Anything you want to remember about ${esc(displayName(p))}">${esc(note?.text || '')}</textarea>
    <div class="button-row"><button class="btn primary" data-action="note-save" data-id="${esc(p.steamId)}">Save note</button>${note ? `<button class="btn ghost" data-action="note-delete" data-id="${esc(p.steamId)}">Delete note</button><span class="muted small">Saved ${new Date(note.updatedAt).toLocaleString()}</span>` : ''}</div>
    <p class="muted small">Personal notes stay on this PC, are marked ✎ beside the player, and are never mixed with measured data.</p>`);
}

export const TABS = [['overview', 'Overview'], ['performance', 'Performance'], ['matches', 'Matches'], ['history', 'History'], ['notes', 'Notes']];

export function renderPlayerDetails(player, state, tab, record) {
  const body = { performance: () => performance(player, state), matches: () => matches(player), history: () => historyTab(player, record), notes: () => notesTab(player, record) }[tab] || (() => overview(player));
  return { identity: identity(player), body: body() };
}

export { premierPeak };
