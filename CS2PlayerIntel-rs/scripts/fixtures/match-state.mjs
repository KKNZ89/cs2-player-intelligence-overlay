// A realistic, entirely fictional app state for screenshots and visual checks: a Premier match on
// Mirage with ten players and the mix of available and missing provider data seen in real lobbies.
// The win estimate as the Rust backend computes it for these players (see src-tauri/src/prediction.rs).
const PREDICTION = { status: 'ok', teamSize: 4, enemySize: 3, inferredEnemies: [], teamRated: 4, enemyRated: 3, teamAverage: 17850, enemyAverage: 19726.666666666668, probability: 0.25344997280212067 };

const now = Date.UTC(2026, 9, 8, 7, 30);
const names = ['You', 'Kestrel', 'nova', 'Brightside', 'mxlk', 'ZeroDay', 'Halcyon', 'r1ft', 'Paxton', 'Lumen'];
const outcomes = 'WWLWLWWLWWLLWLWWLWLW';

function leetify(i) {
  if (i === 4 || i === 8) return { status: 'not-found' };
  const premier = [18420, 20110, 15230, 17640, 0, 22810, 19530, 16840, 0, 14120][i];
  return {
    status: 'ok', fetchedAt: now - 90_000, premier, leetifyRating: [1.21, 2.4, -0.6, 0.9, 0, 3.1, 1.4, 0.2, 0, -1.1][i],
    faceitLevel: [7, 8, 5, 6, null, 10, 7, 6, null, 5][i], faceitElo: [1720, 2010, 1180, 1460, null, 2890, 1750, 1390, null, 1120][i],
    aim: 60 + i * 3, positioning: 48 + i * 2, utility: 40 + i * 4, timeToDamageMs: 540 - i * 9, headAccuracy: 18 + i * 1.3,
    preaim: 9.5 - i * 0.4, winrate: [0.53, 0.58, 0.51, 0.55, 0, 0.64, 0.56, 0.52, 0, 0.5][i], totalMatches: 300 + i * 70,
    ctOpeningDuelSuccess: 48 + i * 1.5, tOpeningDuelSuccess: 46 + i * 1.2, tradeKillsSuccess: 52 + i * 1.4, opening: -0.01 + i * 0.004, clutch: 0.02 + i * 0.003,
    ctLeetify: 0.01 + i * 0.004, tLeetify: 0.01, sprayAccuracy: 38 + i, counterStrafing: 80 + i, mapRanks: [{ map: 'de_mirage', rank: 10 + (i % 8) }],
    recent: Array.from({ length: 24 }, (_, n) => ({ outcome: outcomes[(n + i) % outcomes.length] === 'W' ? 'win' : 'loss',
      map: n % 3 ? 'de_mirage' : 'de_inferno', score: [13, 9 + (n % 4)], finishedAt: new Date(now - n * 864e5).toISOString(), dataSource: n % 5 === 4 ? 'faceit' : 'matchmaking',
      rankType: n % 5 === 4 ? null : 11, rank: n % 5 === 4 ? 7 : premier - n * 85 + ((n * 37) % 300) }))
  };
}

function csrep(i) {
  if (i === 6) return { status: 'verification-required' };
  return {
    status: 'ok', origin: 'public-page', parsing: 'unverified-page-text', fetchedAt: now - 120_000,
    trust: [96, 88, 71, 93, 54, 99, 85, 90, 77, 92][i], trustVerdict: ['EXCELLENT', 'GOOD', 'FAIR', 'EXCELLENT', 'REVIEW', 'EXCELLENT', 'GOOD', 'EXCELLENT', 'FAIR', 'EXCELLENT'][i],
    breakdown: [{ label: 'Statistical Trust', value: '97' }, { label: 'Account Flags', value: '100' }, { label: 'Anomalies', value: '100' }, { label: 'Account Bonus', value: '+6%' }],
    anomalies: [{ label: 'SKILL-GROUP EXPECTATIONS', verdict: 'No Impact' }, { label: 'MATCH MANIPULATION', verdict: 'No Impact' }, { label: 'LOBBY QUALITY', verdict: 'No Impact' }, { label: 'MEDAL BOOSTING', verdict: 'No Impact' }],
    sample: 'Last 60 matches', timeToDamageMs: 520 - i * 8, reactionTimeMs: 330 - i * 4, crosshairPlacement: 8 - i * 0.3,
    kd: [1.21, 1.38, 0.98, 1.12, 0.88, 1.65, 1.24, 1.01, 1.33, 0.91][i], adr: 78 + i * 2.1, aimAccuracy: 17 + i, headAccuracy: 19 + i,
    hltv: [1.12, 1.22, 0.95, 1.05, 0.9, 1.34, 1.15, 1.0, 1.18, 0.93][i], kast: 68 + i
  };
}

function player(i) {
  const isSelf = i === 0;
  return {
    steamId: String(76561198012345670n + BigInt(i)), name: names[i], isSelf,
    side: isSelf || i <= 3 ? 'team' : i >= 5 && i <= 7 ? 'enemy' : '', sideSource: isSelf ? 'self' : i <= 2 ? 'spectated' : i === 3 ? 'likely' : i <= 7 ? 'manual' : '',
    colour: i <= 4 ? ['yellow', 'purple', 'green', 'blue', 'orange'][i] : undefined,
    sideReason: i === 3 ? 'Plays in a party with Kestrel' : '', party: i === 1 ? { friend: true, lobby: '1098', server: '' } : null,
    source: isSelf ? 'gsi' : 'steam-coplay', confidence: isSelf ? 'confirmed' : 'candidate', seenAt: now - 600_000,
    leetify: leetify(i), csrep: csrep(i),
    csstats: i % 3 === 0 ? { status: 'ok', fetchedAt: now - 60_000, kd: [1.19, 0, 0, 1.1, 0, 0, 1.27, 0, 0, 0.94][i], winRate: [53, 0, 0, 55, 0, 0, 56, 0, 0, 50][i], hs: 44 + i, matches: 400 + i * 20 } : { status: 'auth-required' },
    steam: { status: 'ok', privacy: i === 8 ? 'friendsonly' : 'public', vacBanned: false, createdAt: now - (2 + i) * 365 * 864e5, memberSince: 'March 3, 2018',
      hoursCs2: [2450, 3120, 1860, 2100, null, 190, 2780, 1420, null, 760][i], hoursReason: i === 4 || i === 8 ? 'Game details are private' : '', limitedAccount: false, avatar: '' },
    history: isSelf ? { status: 'self' } : { status: 'no-own-history' },
    faceit: { status: 'missing-api-key' },
    analysis: isSelf ? undefined : i === 8
      ? { indicator: 'review', confidence: 'medium', reasons: ['Premier 21000 is well above FACEIT level 2', 'Premier 21000 with 140 CS2 hours on the account'], checked: ['Premier vs FACEIT', 'Premier vs CS2 hours', 'Premier vs account age'], tendencies: [] }
      : i === 4 ? { indicator: 'insufficient', confidence: 'low', reasons: [], checked: [], tendencies: [] }
        : { indicator: 'normal', confidence: i % 2 ? 'high' : 'medium', reasons: [], checked: ['Premier vs FACEIT', 'Premier vs CS2 hours', 'Premier vs account age', 'Leetify aim vs Premier'], tendencies: i === 5 ? ['Usually wins opening duels (56% CT/T average)', 'Stronger on CT than T (Leetify side ratings)'] : [] },
    strongest: i === 5,
    hasNote: i === 6,
    notes: i === 6 ? [{ id: 3, text: 'Rushes B with the AWP every pistol round.', createdAt: now - 3 * 864e5, map: 'de_mirage', mode: 'premier', side: 'enemy', matchId: 42, result: 'loss' }] : [],
    localHistory: isSelf ? undefined : { all: { played: i % 4, won: i % 4 ? 1 : 0, lost: i % 4 > 1 ? 1 : 0, tied: 0, unknown: 0 }, together: { played: 0 }, against: { played: 0 } }
  };
}

const STEAM_READY = { state: 'ready', label: 'Ready', detail: "Reading Steam's recently played list to find the players in your match." };

// Before CS2 reports anything: the dashboard shows the setup guide.
function offlineState() {
  const state = matchState();
  return {
    ...state,
    gsi: { connected: false, error: '', notice: '', map: '', mode: '', phase: '', round: null, roundPhase: '', scoreCT: null, scoreT: null, selfSteamId: '',
      selfName: '', selfTeam: '', matchStatsStatus: 'disconnected', kills: null, assists: null, deaths: null, kd: null, mvps: null, score: null, observed: null },
    match: { state: 'offline', label: 'Waiting for CS2' },
    players: [],
    prediction: { status: 'needs-teams', teamSize: 0, enemySize: 0, inferredEnemies: [] },
    coverage: { players: 0, leetify: { loaded: 0, pending: 0 }, csrep: { loaded: 0, pending: 0 }, csstats: { loaded: 0, pending: 0 }, steam: { loaded: 0, pending: 0 }, lastFetchedAt: null },
    steam: { state: 'waiting', label: 'Waiting for CS2', detail: 'Starts with CS2, so Steam can still launch the game.' },
    rosterStatus: 'Starts with CS2, so Steam can still launch the game.',
    overlayVisible: false,
    setup: { ...state.setup, gsiInstalled: true, cs2Running: false }
  };
}

function matchState({ theme = 'dark', overlayStyle = 'standard', privacyMode = false, overlayView = 'compact', menuOpen = false, overlayLayout = 'right', expandedGroups = ['performance', 'reputation', 'faceit', 'map'] } = {}) {
  const players = names.map((_, i) => player(i));
  return {
    gsi: { connected: true, error: '', notice: '', map: 'de_mirage', mode: 'competitive', phase: 'live', round: 13, roundPhase: 'live', scoreCT: 7, scoreT: 5,
      selfSteamId: players[0].steamId, selfName: 'You', selfTeam: 'CT', matchStatsStatus: 'available', kills: 16, assists: 4, deaths: 11, kd: 16 / 11, mvps: 3, score: 41, observed: null },
    match: { state: 'live', label: 'Live', side: 'CT', mode: 'Competitive', maxPlayers: 10, teams: true, teamSize: 5 },
    players,
    prediction: PREDICTION,
    rosterSelection: 'automatic',
    coverage: { players: 10, leetify: { loaded: 8, pending: 0 }, csrep: { loaded: 9, pending: 0 }, csstats: { loaded: 4, pending: 0 }, steam: { loaded: 10, pending: 0 }, lastFetchedAt: Date.now() - 60_000 },
    overlayView,
    rosterStatus: STEAM_READY.detail,
    steam: STEAM_READY,
    settings: { overlayLayout, overlayTrigger: 'hold-tab', expandedGroups, overlayScale: 1, overlayOpacity: 0.94, overlayX: 0, overlayY: 36, overlayDisplay: '',
      csstatsEnabled: true, csrepPagesEnabled: true, launchCs2OnStart: true, closeToTray: true, startWithWindows: false, theme, overlayStyle, privacyMode,
      overlayColumns: ['hours', 'premier', 'faceit', 'aim', 'ttd', 'hs', 'profile'], profileIndicators: true, showWinEstimate: false, minSampleMatches: 30,
      notifications: true, notifyTypes: ['encounter', 'review', 'provider'], retentionDays: 0, hasFaceitApiKey: true, keyFingerprints: { faceitApiKey: '3f9a-41c2-0be7', steamWebApiKey: null, csrepApiKey: null }, interactHotkey: 'Shift+F8', autoRetryMinutes: 2, hasCsrepApiKey: false, hasSteamWebApiKey: false, steamworksSdkPath: '', cs2CfgPath: '',
      cs2ConsoleLogPath: '', updateSource: '', defaultUpdateSource: 'https://github.com/KKNZ89/cs2-player-intelligence-overlay/releases/latest/download/latest.json', warnings: [] },
    overlayVisible: true,
    displays: [{ id: '1', label: 'Primary display' }],
    version: '1.0.0',
    updates: { status: 'up-to-date', currentVersion: '1.0.0', version: '', percent: null, message: '', source: '' },
    setup: { shortcutReady: true, hotkey: 'F8', menuOpen, diagnosticsFile: 'C:\\Users\\you\\AppData\\Roaming\\cs2-player-intel\\diagnostics.log', cfgFound: true, gsiInstalled: true, cs2Running: true,
      csrep: 'public profile pages (no key)', csstats: 'enabled (verify with a lookup)', csstatsSignIn: { signedIn: true, checkedAt: Date.now() - 240_000 }, gsiConfig: '', game: '', warnings: [] }
  };
}

export { matchState, offlineState };
