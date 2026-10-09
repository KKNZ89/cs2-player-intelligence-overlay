import test from 'node:test';
import assert from 'node:assert/strict';
import { colourClass, discovery, groups, issuesHtml, lobbyCount, matchTitle, playerChips, providerIssues, rosterCounts, winChance } from '../ui/lib/format.js';
import { providerStatusText } from '../ui/lib/status.js';

const players = count => Array.from({ length: count }, (_, i) => ({ steamId: String(76561198000000000n + BigInt(i)), name: `P${i}`, isSelf: i === 0, side: i === 0 ? 'team' : '' }));

test('deathmatch lobbies show every player in one group with the mode lobby size', () => {
  const state = { players: players(14), match: { teams: false, maxPlayers: 16 }, prediction: { status: 'ffa' } };
  const result = groups(state);
  assert.equal(result.length, 1);
  assert.equal(result[0].title, 'Players');
  assert.equal(result[0].players.length, 14);
  assert.equal(lobbyCount(state), '14/16');
  assert.equal(winChance(state.prediction).text, '—');
});

test('team modes group by side, cap at the lobby size, and label CT/T from your side', () => {
  const state = { players: players(12), match: { teams: true, maxPlayers: 10, side: 'T' }, prediction: {} };
  const result = groups(state);
  assert.deepEqual(result.map(group => group.title), ['T · Your team', 'Not assigned']);
  assert.equal(result.reduce((sum, group) => sum + group.players.length, 0), 10, 'never more than the lobby holds');
  assert.equal(lobbyCount(state), '10/10');
});

test('unknown teams explain manual assignment without assigning free-for-all players a team', () => {
  const state = { players: players(3), match: { teams: true, maxPlayers: 10 } };
  const unknown = groups(state).find(group => group.side === 'unknown');
  assert.equal(unknown.players.length, 2);
  assert.match(unknown.description, /Team not known/);
  assert.match(unknown.description, /Teammate\/Opponent/);
  assert.equal(groups({ ...state, match: { teams: false, maxPlayers: 16 } })[0].description, undefined);
});

test('manual side choices take priority over inferred opponents', () => {
  const roster = players(3);
  roster[1].side = 'team';
  roster[2].side = 'enemy';
  const state = { players: roster, match: { teams: true, maxPlayers: 10 }, prediction: { inferredEnemies: [roster[1].steamId] } };
  const result = groups(state);
  assert.deepEqual(result.find(group => group.side === 'team').players.map(player => player.steamId), roster.slice(0, 2).map(player => player.steamId));
  assert.deepEqual(result.find(group => group.side === 'enemy').players.map(player => player.steamId), [roster[2].steamId]);
  assert.equal(result.some(group => group.side === 'unknown'), false);
});

test('player badges explain how a side was decided', () => {
  assert.match(playerChips({ sideSource: 'spectated' }), /Spectated/);
  assert.match(playerChips({ sideSource: 'likely', sideReason: 'Plays in a party with Kestrel' }), /Likely · party[\s\S]*|title="Plays in a party with Kestrel"/);
  assert.match(playerChips({ party: { friend: true, lobby: '1' } }), /Friend/);
  assert.match(playerChips({}, { inferred: true }), /Inferred/);
  assert.equal(playerChips({}), '');
});

test('match titles name the mode and map without repeating mode-specific maps', () => {
  assert.equal(matchTitle({ connected: true, mode: 'competitive', map: 'de_mirage' }), 'Competitive · Mirage');
  assert.equal(matchTitle({ connected: true, mode: 'rush', map: 'rush_001' }), 'Rush');
  assert.equal(matchTitle({ connected: false }), 'Waiting for CS2');
});

test('discovery separates confirmed players from possible ones', () => {
  const now = Date.now();
  const state = { match: { teams: true, mode: 'Competitive', maxPlayers: 10 }, players: [
    { steamId: '1', isSelf: true },
    { steamId: '2', source: 'gsi-spectated', sideSource: 'spectated' },
    { steamId: '3', source: 'manual', confidence: 'user-selected' },
    { steamId: '4', source: 'steam-coplay', seenAt: now - 120_000 }
  ] };
  assert.deepEqual(rosterCounts(state), { confirmed: 3, possible: 1, max: 10 });
  assert.match(discovery(state.players[3], state).label, /^Recently played · 2 min ago$/);
  const deathmatch = { match: { teams: false, mode: 'Deathmatch' } };
  assert.match(discovery({ source: 'steam-coplay', seenAt: now - 15 * 60_000 }, deathmatch).label, /may have left/);
  assert.match(playerChips(state.players[3], { state }), /chip possible/);
});

test('provider problems are listed beside the player', () => {
  const player = { leetify: { status: 'not-found' }, csrep: { status: 'verification-required' }, csstats: { status: 'ok', stale: true, staleReason: 'timeout' }, steam: { status: 'ok' } };
  assert.deepEqual(providerIssues(player).map(note => `${note.provider}:${note.text}`), ['leetify:no profile', 'csrep:security check', 'csstats:out of date']);
  assert.match(issuesHtml(player), /<b>CSRep<\/b> security check/);
  assert.equal(issuesHtml({ leetify: { status: 'ok' } }), '');
});

test('cookie consent statuses direct users to choose manually and refresh', () => {
  const result = { status: 'consent-required' };
  assert.match(providerStatusText(result), /choose cookie preferences, then refresh/);
  assert.deepEqual(providerIssues({ csrep: result }).map(issue => issue.text), ['cookie preferences needed']);
  assert.match(issuesHtml({ csrep: result }), /cookie preferences needed/);
  assert.match(providerStatusText({ status: 'verification-required' }), /complete verification, then refresh/);
});

test('frequent partners are shown as information', () => {
  assert.match(playerChips({ partners: [{ steamId: '9', name: 'Kestrel', count: 4 }] }), /Often with Kestrel/);
});

test('source priority decides which source a shared value comes from', async () => {
  const { columns, setPriority } = await import('../ui/lib/format.js');
  const player = { csstats: { status: 'ok', kd: 1.5 }, faceit: { status: 'ok', kd: 1.2, winRate: 60 }, leetify: { status: 'ok', winrate: 0.51 } };
  setPriority(['csstats', 'faceit', 'leetify', 'csrep']);
  assert.match(columns.kd(player), />1\.50</);
  setPriority(['faceit', 'csstats', 'leetify', 'csrep']);
  assert.match(columns.kd(player), />1\.20</);
  assert.match(columns.winRate(player), />60%</);
  setPriority(['leetify', 'faceit', 'csstats', 'csrep']);
  assert.match(columns.winRate(player), />51%</, 'Leetify fraction shown as a percentage');
  setPriority(['faceit', 'leetify', 'csstats', 'csrep']);
});

test('teammate colours become ring classes; anything else is ignored', () => {
  assert.equal(colourClass({ colour: 'purple' }), ' tc tc-purple');
  assert.equal(colourClass({}), '');
  assert.equal(colourClass({ colour: 'red" onload="x' }), '', 'only the five CS2 colours reach the markup');
});
