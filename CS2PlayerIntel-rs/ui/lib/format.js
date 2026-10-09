// Value formatting shared by the dashboard and the overlay.
// - Metric names follow each provider's own labels (Leetify's guidelines forbid renaming or rescaling).
// - Every value names its source on hover. Values read from page text (CSRep and CSStats pages) carry
//   a dotted underline: they are scraped estimates, not API data.
// - Missing data is N/A with the reason on hover; never zero, and a missing CSRep result never reads clean.
import { esc } from './dom.js';
import { providerStatusText } from './status.js';
import { icon } from './icons.js';

export const isNum = value => typeof value === 'number' && Number.isFinite(value);
const NAMES = { leetify: 'Leetify', csrep: 'CSRep', csstats: 'CSStats', steam: 'Steam', faceit: 'FACEIT', local: 'This app' };

// Streaming privacy mode: players become "Player 1", "Player 2"… and SteamIDs and avatars are hidden.
let privacy = null;
export function setPrivacy(on, players = []) {
  privacy = on ? new Map(players.map((p, i) => [p.steamId, p.isSelf ? 'You' : `Player ${i + 1}`])) : null;
}
export const privacyOn = () => privacy !== null;
export function displayName(player) {
  return privacy ? privacy.get(player.steamId) || 'Player' : player.name || player.steamId;
}
/** The SteamID64, or nothing in privacy mode. */
export function shownId(player) {
  return privacy ? '' : player.steamId || '';
}

function reason(player, provider) {
  const result = player[provider];
  if (!result || result.status === 'pending') return `${NAMES[provider]}: loading`;
  if (result.status === 'ok') return `${NAMES[provider]}: not published for this player`;
  return `${NAMES[provider]}: ${providerStatusText(result)}`;
}

export function na(why, { loading = false } = {}) {
  return loading ? `<span class="na loading" title="${esc(why)}"><i></i></span>` : `<span class="na" title="${esc(why)}">N/A</span>`;
}

const isLoading = (player, providers) => providers.some(provider => !player[provider] || player[provider].status === 'pending');

// First provider with a value wins.
// Source priority (Settings → Data sources): when several sources publish the same kind of value, the
// earliest in this order that has one wins. Sources not listed keep their given order after it.
let priority = ['faceit', 'leetify', 'csstats', 'csrep'];
export function setPriority(order) {
  if (Array.isArray(order) && order.length) priority = order;
}
const rank = provider => {
  const index = priority.indexOf(provider);
  return index < 0 ? priority.length : index;
};
export const byPriority = providers => [...providers].sort((a, b) => rank(a) - rank(b));

/** The value from the highest-priority source that has one: pairs of [provider, key, scale?]. */
export function valueOf(player, pairs) {
  for (const [provider, key, scale] of [...pairs].sort((a, b) => rank(a[0]) - rank(b[0]))) {
    const value = player[provider]?.[key];
    if (isNum(value)) return scale ? value * scale : value;
  }
  return null;
}

/** Shared value definitions, so tables, sorting and averages agree on the source. */
export const SHARED = {
  kd: [['csstats', 'kd'], ['csrep', 'kd'], ['faceit', 'kd']],
  win: [['csstats', 'winRate'], ['faceit', 'winRate'], ['leetify', 'winrate', 100]],
  faceitLevel: [['faceit', 'level'], ['leetify', 'faceitLevel']],
  faceitElo: [['faceit', 'elo'], ['leetify', 'faceitElo']],
  ttd: [['leetify', 'timeToDamageMs'], ['csrep', 'timeToDamageMs']]
};

function pick(player, key, providers) {
  for (const provider of byPriority(providers)) {
    const value = player[provider]?.[key];
    if (isNum(value)) return { value, provider, result: player[provider] };
  }
  return null;
}

function valueSpan(found, html) {
  const { provider, result } = found;
  const parts = [NAMES[provider]];
  if (result.fetchedAt) parts.push(`fetched ${new Date(result.fetchedAt).toLocaleTimeString()}`);
  if (result.parsing) parts.push('read from page text (estimate)');
  if (result.stale) parts.push(`stale: ${result.staleReason}`);
  const classes = ['v', result.parsing ? 'est' : '', result.stale ? 'stale' : ''].filter(Boolean).join(' ');
  return `<span class="${classes}" title="${esc(parts.join(' · '))}">${html}</span>`;
}

// First (provider, key) pair with a value wins, for values different providers name differently.
function showAny(player, pairs, render) {
  for (const [provider, key, scale] of [...pairs].sort((a, b) => rank(a[0]) - rank(b[0]))) {
    const value = player[provider]?.[key];
    if (isNum(value)) return valueSpan({ value, provider, result: player[provider] }, render(scale ? value * scale : value));
  }
  const providers = [...new Set(pairs.map(([provider]) => provider))];
  return na(providers.map(provider => reason(player, provider)).join(' · '), { loading: isLoading(player, providers) });
}

function show(player, key, providers, render) {
  const found = pick(player, key, providers);
  if (!found) return na(providers.map(provider => reason(player, provider)).join(' · '), { loading: isLoading(player, providers) });
  return valueSpan(found, render(found.value));
}

// CS2 Premier colour bands.
export function premierTier(rating) {
  if (!isNum(rating) || rating <= 0) return 'none';
  if (rating >= 30000) return 'gold';
  if (rating >= 25000) return 'red';
  if (rating >= 20000) return 'pink';
  if (rating >= 15000) return 'purple';
  if (rating >= 10000) return 'blue';
  if (rating >= 5000) return 'cyan';
  return 'grey';
}

const premierBadge = (value, text) => `<span class="premier tier-${premierTier(value)}">${text}</span>`;

export const columns = {
  premier: p => show(p, 'premier', ['leetify'], value => premierBadge(value, value >= 1000 ? `${(value / 1000).toFixed(1)}k` : String(value))),
  premierFull: p => show(p, 'premier', ['leetify'], value => premierBadge(value, value.toLocaleString('en-US'))),
  kd: p => show(p, 'kd', ['csstats', 'csrep', 'faceit'], value => value.toFixed(2)),
  // CSStats publishes a percentage, Leetify a 0–1 fraction (shown as Leetify shows it, a percentage).
  // CSStats and FACEIT publish a percentage, Leetify a 0–1 fraction (shown as a percentage, as Leetify does).
  winRate: p => showAny(p, [['csstats', 'winRate'], ['faceit', 'winRate'], ['leetify', 'winrate', 100]], value => `${Math.round(value)}%`),
  faceit: p => showAny(p, [['faceit', 'level'], ['leetify', 'faceitLevel']], value => `<span class="faceit lvl-${Math.max(1, Math.min(10, value))}">${value}</span>`),
  faceitElo: p => showAny(p, [['faceit', 'elo'], ['leetify', 'faceitElo']], value => value.toLocaleString('en-US')),
  faceitMatches: p => showAny(p, [['faceit', 'matches']], value => value.toLocaleString('en-US')),
  // Headshot kills (CSStats, FACEIT). Leetify's "Head Accuracy" is a different measure and stays separate.
  hs: p => showAny(p, [['csstats', 'hs'], ['faceit', 'hs']], value => `${Math.round(value)}%`),
  premierPeak: p => {
    const peak = premierPeak(p);
    return peak ? `<span class="v" title="Highest Premier rating in their last ${peak.count} Premier matches on Leetify">${premierBadge(peak.value, peak.value.toLocaleString('en-US'))}</span>` : na(p.leetify?.status === 'ok' ? 'Leetify: no recent Premier matches' : reason(p, 'leetify'), { loading: isLoading(p, ['leetify']) });
  },
  accountAgeShort: p => {
    const s = p.steam;
    if (!s || s.status !== 'ok' || !isNum(s.createdAt)) return na(reason(p, 'steam'), { loading: isLoading(p, ['steam']) });
    const years = (Date.now() - s.createdAt) / (365.25 * 864e5);
    return `<span class="v" title="Steam member since ${esc(s.memberSince)}">${years >= 1 ? `${Math.floor(years)}y` : `${Math.max(1, Math.round(years * 12))}mo`}</span>`;
  },
  hours: p => {
    const steam = p.steam;
    if (isNum(steam?.hoursCs2)) return show(p, 'hoursCs2', ['steam'], value => value.toLocaleString('en-US'));
    if (steam?.status === 'ok') return na(`Steam: ${steam.hoursReason || 'not public'}`);
    return na(reason(p, 'steam'), { loading: isLoading(p, ['steam']) });
  },
  csrep: p => {
    const r = p.csrep;
    if (!r || r.status === 'pending') return na('CSRep: loading', { loading: true });
    if (r.status !== 'ok' || !isNum(r.trust)) return na(`${reason(p, 'csrep')}. No CSRep result is not the same as a clean record.`);
    const level = r.trust >= 80 ? 'good' : r.trust >= 60 ? 'warn' : 'bad';
    const verdict = r.trustVerdict ? ` · CSRep verdict: ${r.trustVerdict.toLowerCase()}` : '';
    return valueSpan({ provider: 'csrep', result: r }, `<span class="trust ${level}">${Math.round(r.trust)}%</span>`)
      .replace('title="', `title="CSRep Trust Score${verdict} · CSRep's own assessment, not a cheating verdict · `);
  },
  rating: p => show(p, 'hltv', ['csrep', 'csstats'], value => value.toFixed(2)),
  adr: p => show(p, 'adr', ['csrep', 'csstats'], value => value.toFixed(1)),
  hsKills: p => showAny(p, [['csstats', 'hs'], ['faceit', 'hs']], value => `${Math.round(value)}%`),
  headAccuracy: p => show(p, 'headAccuracy', ['leetify', 'csrep'], value => `${value.toFixed(1)}%`),
  timeToDamage: p => show(p, 'timeToDamageMs', ['leetify', 'csrep'], value => `${Math.round(value)}<small>ms</small>`),
  matches: p => show(p, 'totalMatches', ['leetify'], value => value.toLocaleString('en-US')),
  leetifyRating: p => show(p, 'leetifyRating', ['leetify'], value => `${value > 0 ? '+' : ''}${value.toFixed(2)}`),
  aim: p => show(p, 'aim', ['leetify'], value => String(Math.round(value))),
  positioning: p => show(p, 'positioning', ['leetify'], value => String(Math.round(value))),
  utility: p => show(p, 'utility', ['leetify'], value => String(Math.round(value))),
  preaim: p => show(p, 'preaim', ['leetify'], value => `${value.toFixed(1)}°`),
  last10: p => {
    const recent = p.leetify?.recent;
    if (!Array.isArray(recent) || !recent.length) return na(p.leetify?.status === 'ok' ? 'Leetify: no recent matches' : reason(p, 'leetify'), { loading: isLoading(p, ['leetify']) });
    const letter = { win: 'W', loss: 'L', tie: 'T' };
    return `<span class="form" title="Last ${Math.min(10, recent.length)} Leetify matches, newest first">${recent.slice(0, 10).map(m => `<i class="${m.outcome}">${letter[m.outcome] || '?'}</i>`).join('')}</span>`;
  },
  vac: p => {
    const s = p.steam;
    if (!s || s.status !== 'ok' || s.vacBanned === null || s.vacBanned === undefined) return na(reason(p, 'steam'), { loading: isLoading(p, ['steam']) });
    const games = isNum(s.gameBans) ? ` · ${s.gameBans} game ban${s.gameBans === 1 ? '' : 's'}` : '';
    if (s.vacBanned || s.gameBans > 0) return `<span class="ban" title="Public Steam ban record">On record${isNum(s.vacBans) ? ` (${s.vacBans} VAC${games})` : ''}</span>`;
    return `<span class="muted" title="Steam reports no public VAC ban${isNum(s.gameBans) ? ' and no game bans' : ''}">None public</span>`;
  },
  accountAge: p => {
    const s = p.steam;
    if (!s || s.status !== 'ok' || !isNum(s.createdAt)) return na(s?.status === 'ok' ? 'Steam: account age not public' : reason(p, 'steam'), { loading: isLoading(p, ['steam']) });
    const years = (Date.now() - s.createdAt) / (365.25 * 864e5);
    return `<span class="v" title="Steam member since ${esc(s.memberSince)}">${years >= 1 ? `${Math.floor(years)} yr` : `${Math.max(1, Math.round(years * 12))} mo`}</span>`;
  }
};

// Matches on a map from the player's recent Leetify matches (counts only; nothing is re-scored).
export function mapRecord(p, map) {
  const recent = p.leetify?.recent;
  if (!map) return na('No current map');
  if (!Array.isArray(recent)) return na(reason(p, 'leetify'), { loading: isLoading(p, ['leetify']) });
  const games = recent.filter(m => m.map === map);
  if (!games.length) return `<span class="muted" title="No ${esc(map)} games in their last ${recent.length} Leetify matches">0</span>`;
  const won = games.filter(m => m.outcome === 'win').length;
  return `<span class="v" title="Last ${recent.length} Leetify matches">${games.length} · ${won}–${games.filter(m => m.outcome === 'loss').length}</span>`;
}

function record(bucket) {
  const decided = bucket.won + bucket.lost + bucket.tied;
  const rate = decided ? ` <span class="muted">${Math.round((bucket.won / decided) * 100)}%</span>` : '';
  return `${bucket.won}–${bucket.lost}${bucket.tied ? `–${bucket.tied}` : ''}${rate}`;
}

// "Played before": Leetify history when both players have it, otherwise this app's own records.
export function history(player, { compact = false } = {}) {
  if (player.isSelf) return '<span class="muted">—</span>';
  const h = player.history;
  if (h?.status === 'ok' && (h.together.played || h.against.played)) {
    const parts = [];
    if (h.together.played) parts.push(`<span class="with">${compact ? 'W/' : 'With'} ${h.together.played}× ${record(h.together)}</span>`);
    if (h.against.played) parts.push(`<span class="vs">${compact ? 'vs' : 'Against'} ${h.against.played}× ${record(h.against)}</span>`);
    return `<span class="v" title="Shared matches in both players' recent Leetify history, with your results">${parts.join(compact ? ' ' : '<br>')}</span>`;
  }
  const local = player.localHistory;
  if (local?.all?.played) {
    const sides = [local.together.played ? `with ${local.together.played}` : '', local.against.played ? `vs ${local.against.played}` : ''].filter(Boolean).join(', ');
    return `<span class="v" title="Matches recorded by this app${sides ? ` (${sides})` : ''}">${compact ? '' : 'Seen '}${local.all.played}× ${record(local.all)}</span>`;
  }
  if (h?.status === 'ok') return '<span class="muted" title="No shared matches in recent Leetify history or this app\'s records">Never</span>';
  if (h?.status === 'pending') return na('Checking played-before', { loading: true });
  return '<span class="muted" title="Not in this app\'s records; Leetify history unavailable">Not seen</span>';
}

// The win estimate is this app's own unvalidated guess from average Premier rating, so it is labelled
// and shown modestly.
export function winChance(prediction = {}) {
  if (prediction.status === 'ffa') return { percent: null, text: '—', detail: 'Free-for-all: no teams to compare' };
  if (prediction.status === 'ok') {
    const percent = Math.round(prediction.probability * 100);
    return { percent, text: `${percent}%`, detail: `Avg Premier ${Math.round(prediction.teamAverage).toLocaleString('en-US')} vs ${Math.round(prediction.enemyAverage).toLocaleString('en-US')}` };
  }
  if (prediction.status === 'not-enough-data') {
    return { percent: null, text: '—', detail: `Needs Premier ratings for 2+ players per team (${prediction.teamRated ?? 0} vs ${prediction.enemyRated ?? 0})` };
  }
  return { percent: null, text: '—', detail: 'Mark teammates and opponents to estimate' };
}

const TEAMMATE_COLOURS = ['yellow', 'purple', 'green', 'blue', 'orange'];

// Classes for the avatar ring when CS2's teammate colour is known (read from the scoreboard).
export function colourClass(player) {
  return TEAMMATE_COLOURS.includes(player.colour) ? ` tc tc-${player.colour}` : '';
}

// Steam avatar when public, otherwise an initial on a colour derived from the SteamID.
export function avatar(player, size = 28) {
  const url = player.steam?.avatar;
  if (!privacy && url && /^https:\/\/avatars\.(akamai\.|cloudflare\.|fastly\.)?steamstatic\.com\//.test(url)) {
    return `<img class="avatar" src="${esc(url)}" alt="" width="${size}" height="${size}">`;
  }
  // A class per colour: the strict CSP forbids inline style attributes.
  const hue = Number(BigInt(/^\d+$/.test(player.steamId || '') ? player.steamId : '0') % 12n);
  const initial = esc((privacy ? displayName(player).replace('Player ', '') : player.name || '?').trim().charAt(0).toUpperCase() || '?');
  return `<span class="avatar initial hue-${hue}">${initial}</span>`;
}

export function playerName(player) {
  return `${esc(displayName(player))}${player.isSelf && !privacy ? ' <span class="you">YOU</span>' : ''}`;
}

// Highest Premier rating among their recent Leetify Premier matches (rank type 11), with the sample.
export function premierPeak(player) {
  const ranks = (player.leetify?.recent || []).filter(m => m.rankType === 11 && isNum(m.rank) && m.rank > 0).map(m => m.rank);
  return ranks.length ? { value: Math.max(...ranks), count: ranks.length } : null;
}

// The profile indicator: Normal / Review / Insufficient data, with the reasons on hover.
const INDICATOR = { normal: ['Normal', 'normal'], review: ['Review', 'review'], insufficient: ['Not enough data', 'insufficient'] };
export function profileIndicator(player, { long = false, short = false } = {}) {
  const a = player.analysis;
  if (player.isSelf) return '<span class="muted">—</span>';
  if (!a) return na('Not analysed yet', { loading: true });
  const [full, cls] = INDICATOR[a.indicator] || INDICATOR.insufficient;
  const label = short && a.indicator === 'insufficient' ? 'N/A' : full;
  const checked = a.checked?.length ? `Compared: ${a.checked.join(', ')}.` : 'Too few values to compare.';
  const why = a.reasons?.length ? `${a.reasons.join('; ')}. ` : '';
  const title = `${why}${checked} ${a.confidence} confidence. A statistical comparison, not evidence of cheating.`;
  return `<span class="indicator ${cls}" title="${esc(title)}">${label}${long && a.indicator !== 'insufficient' ? ` · ${a.confidence} confidence` : ''}</span>`;
}

// Small markers beside a name: strongest by Premier, met before, profile review, your note.
// `history: false` (the overlay) leaves out the met-before count: match history is shown in the app only.
export function statusIcons(player, { indicators = true, history = true } = {}) {
  const icons = [];
  if (player.strongest) icons.push('<span class="st strongest" title="Highest Premier rating in this lobby">★</span>');
  const met = history && player.localHistory?.all?.played;
  if (met) icons.push(`<span class="st seen" title="Met in ${met} earlier match${met === 1 ? '' : 'es'} recorded by this app">↺${met}</span>`);
  if (indicators && player.analysis?.indicator === 'review') icons.push(`<span class="st review" title="${esc(`Profile review: ${player.analysis.reasons.join('; ')}. A statistical discrepancy, not evidence of cheating.`)}">⚑</span>`);
  if (player.hasNote) icons.push(`<span class="st note" title="You have a note on this player">${icon('note', { size: 12 })}</span>`);
  return icons.length ? `<span class="status-icons">${icons.join('')}</span>` : '';
}

export function sideOf(player, inferred) {
  return player.isSelf || player.side === 'team' ? 'team' : player.side === 'enemy' || inferred.has(player.steamId) ? 'enemy' : 'unknown';
}

// Players grouped by side with CT/T from your current side; the order inside a group is the service's.
// Free-for-all modes (deathmatch) have a single group. The limit follows the mode's lobby size.
export function groups(state, limit = state.match?.maxPlayers || 10) {
  const players = (state.players || []).slice(0, limit);
  if (state.match?.teams === false) return players.length ? [{ side: 'unknown', teamSide: '', title: 'Players', players, inferred: new Set(), ffa: true }] : [];
  const inferred = new Set(state.prediction?.inferredEnemies || []);
  const own = state.match?.side;
  const other = own === 'CT' ? 'T' : own === 'T' ? 'CT' : '';
  return [['team', own, 'Your team'], ['enemy', other, 'Opponents'], ['unknown', '', 'Not assigned']]
    .map(([side, teamSide, title]) => ({
      side, teamSide, title: teamSide ? `${teamSide} · ${title}` : title,
      description: side === 'unknown' ? 'Team not known. Steam recent players do not identify teams. Choose Teammate/Opponent in the dashboard, or Team/Enemy in the Esc-menu player card.' : '',
      players: players.filter(p => sideOf(p, inferred) === side), inferred
    }))
    .filter(group => group.players.length);
}

// Team averages of what is known (labelled as averages of available values).
export function teamAverages(players) {
  const avg = values => (values.length ? values.reduce((a, b) => a + b, 0) / values.length : null);
  const premier = avg(players.map(p => p.leetify?.premier).filter(v => isNum(v) && v > 0));
  const kd = avg(players.map(p => valueOf(p, SHARED.kd)).filter(isNum));
  return [premier ? `Avg Premier ${Math.round(premier).toLocaleString('en-US')}` : '', kd ? `Avg K/D ${kd.toFixed(2)}` : ''].filter(Boolean).join(' · ');
}

// How a player got into the list, and how sure that is. Only you, players you spectated and players you
// added are confirmed; Steam's recently played list and console sightings are possible players.
const DROP_IN_STALE_MS = 10 * 60_000;
export function discovery(player, state = {}) {
  if (player.isSelf) return { kind: 'confirmed', label: '', title: '' };
  if (player.source === 'gsi-spectated' || player.sideSource === 'spectated') return { kind: 'confirmed', label: 'Spectated', title: 'Confirmed: you spectated this player in this match' };
  if (player.confidence === 'user-selected' || player.source === 'manual') return { kind: 'confirmed', label: 'Added by you', title: 'You added this player' };
  const when = player.seenAt ? ago(player.seenAt) : '';
  if (player.source === 'console-status') return { kind: 'possible', label: 'Console', title: `Seen in CS2's status output${when ? ` ${when}` : ''}; not confirmed for this match` };
  const dropIn = state.match?.teams === false || state.match?.mode === 'Casual';
  if (dropIn && player.seenAt && Date.now() - player.seenAt > DROP_IN_STALE_MS) {
    return { kind: 'possible', label: `Joined ${when} · may have left`, title: 'Players come and go in this mode; Steam does not report when someone leaves' };
  }
  if (player.source === 'steam-coplay') return { kind: 'possible', label: `Recently played${when ? ` · ${when}` : ''}`, title: "From Steam's recently played list: probably in this match, not confirmed" };
  return { kind: 'possible', label: '', title: '' };
}

export function rosterCounts(state) {
  const players = state.players || [];
  const confirmed = players.filter(p => discovery(p, state).kind === 'confirmed').length;
  return { confirmed, possible: players.length - confirmed, max: state.match?.maxPlayers || 10 };
}

// Short notes on providers that gave no data, shown beside the player instead of only on hover.
const ISSUE_TEXT = {
  'verification-required': 'security check', 'consent-required': 'cookie preferences needed', 'auth-required': 'sign-in needed', 'rate-limited': 'rate limited', timeout: 'timed out',
  error: 'failed', paused: 'paused', 'not-found': 'no profile', 'no-stats': 'no stats', 'auth-failed': 'check API key'
};
export function providerIssues(player) {
  const notes = [];
  for (const provider of ['leetify', 'csrep', 'csstats', 'steam']) {
    const result = player[provider];
    if (!result) continue;
    if (result.stale) notes.push({ provider, text: 'out of date', title: `${NAMES[provider]}: refresh failed (${result.staleReason}); showing earlier data` });
    else if (ISSUE_TEXT[result.status]) notes.push({ provider, text: ISSUE_TEXT[result.status], title: `${NAMES[provider]}: ${result.error || providerStatusText(result)}` });
  }
  return notes;
}

export function issuesHtml(player) {
  const notes = providerIssues(player);
  return notes.length
    ? `<div class="issues">${notes.map(n => `<span title="${esc(n.title)}"><b>${NAMES[n.provider]}</b> ${esc(n.text)}</span>`).join('')}</div>`
    : '';
}

// Small badges under a player's name: how they were found, why they are on a side, who they often play with.
export function playerChips(player, { inferred = false, state = {} } = {}) {
  const chips = [];
  const found = discovery(player, state);
  if (found.label && player.sideSource !== 'spectated') chips.push(`<span class="chip ${found.kind === 'possible' ? 'possible' : ''}" title="${esc(found.title)}">${esc(found.label)}</span>`);
  if (player.sideSource === 'spectated') chips.push('<span class="chip ct" title="Confirmed: you spectated this player after dying">Spectated</span>');
  if (player.sideSource === 'likely') chips.push(`<span class="chip likely" title="${esc(player.sideReason || 'In the same Steam party as a known player')}">Likely · party</span>`);
  if (player.party?.friend) chips.push(`<span class="chip" title="Your Steam friend${player.party.lobby ? ', in a party' : ''}">Friend</span>`);
  if (inferred) chips.push('<span class="chip t" title="The remaining players once your whole team is known">Inferred</span>');
  const partners = player.partners || [];
  if (partners.length) {
    const names = partners.map(p => `${p.name} (${p.count}×)`).join(', ');
    chips.push(`<span class="chip info" title="Often on the same team in recent Leetify matches: ${esc(names)}. Information only; it does not set a side.">Often with ${esc(partners[0].name)}${partners.length > 1 ? ` +${partners.length - 1}` : ''}</span>`);
  }
  return chips.join('');
}

export function lobbyCount(state) {
  const count = (state.players || []).length;
  const max = state.match?.maxPlayers || 10;
  return `${Math.min(count, max)}/${max}`;
}

export function ago(time) {
  if (!time) return 'never';
  const seconds = Math.max(0, Math.round((Date.now() - time) / 1000));
  return seconds < 60 ? `${seconds}s ago` : seconds < 3600 ? `${Math.round(seconds / 60)} min ago` : `${Math.round(seconds / 3600)} h ago`;
}

const MODES = { competitive: 'Competitive', premier: 'Premier', casual: 'Casual', deathmatch: 'Deathmatch', wingman: 'Wingman', scrimcomp2v2: 'Wingman', rush: 'Rush', gungameprogressive: 'Arms Race', armsrace: 'Arms Race' };
export function matchTitle(g) {
  if (!g.connected) return 'Waiting for CS2';
  // Mode-specific maps (rush_001) repeat the mode name, so only the mode is shown for them.
  const map = g.mode && (g.map || '').startsWith(`${g.mode}_`) ? '' : (g.map || '').replace(/^(de|cs|ar|dz)_/, '');
  return [MODES[g.mode] || g.mode, map.charAt(0).toUpperCase() + map.slice(1)].filter(Boolean).join(' · ') || 'In menus';
}

export function roundText(g) {
  if (!g.connected || !g.round) return '';
  const phase = { freezetime: 'Freeze time', live: 'Live', over: 'Round over' }[g.roundPhase] || '';
  return `Round ${g.round}${phase ? ` · ${phase}` : ''}`;
}

// Score markup: CT and T keep their own colours; scores follow the sides.
export function scoreHtml(g) {
  if (!g.connected || g.scoreCT === null || g.scoreCT === undefined) return '<span class="score-empty">—</span>';
  return `<span class="side-ct">CT</span><b class="side-ct">${g.scoreCT}</b><span class="colon">:</span><b class="side-t">${g.scoreT}</b><span class="side-t">T</span>`;
}
