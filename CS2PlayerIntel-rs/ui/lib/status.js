// Human-readable provider and roster statuses.

const LABELS = {
  ok: 'Loaded', pending: 'Loading…', disabled: 'Off in Settings',
  'missing-api-key': 'Add API key', 'auth-failed': 'Check API key',
  'auth-required': 'Sign in needed', 'verification-required': 'Open the profile, complete verification, then refresh',
  'consent-required': 'Open the profile, choose cookie preferences, then refresh',
  'rate-limited': 'Rate limited; retrying', 'not-found': 'No profile',
  'no-stats': 'No readable stats', timeout: 'Timed out', error: 'Failed',
  cancelled: 'Cancelled', 'waiting-for-self': 'Waiting for your SteamID', self: 'You',
  'no-own-history': 'You have no Leetify history', paused: 'Paused after repeated failures'
};

export function providerStatusText(result) {
  const status = result?.status || 'pending';
  return LABELS[status] || status.replaceAll('-', ' ');
}

export function rosterSummaryText(state) {
  const players = state.players || [];
  const automatic = players.filter(p => p.confidence === 'candidate').length;
  const selected = players.filter(p => p.confidence === 'user-selected').length;
  return `${players.length} players · ${automatic} found automatically · ${selected} added`;
}

export function updateText(u = {}) {
  switch (u.status) {
    case 'disabled': return 'Automatic updates run only in the installed app.';
    case 'not-configured': return 'No update source. Leave it blank for GitHub releases, or choose a release folder or https:// URL.';
    case 'checking': return 'Checking for updates…';
    case 'up-to-date': return `Up to date (v${u.currentVersion}).${u.message ? ` ${u.message}` : ''}`;
    case 'downloading': return `Downloading v${u.version}${u.percent != null ? ` (${u.percent}%)` : ''}…`;
    case 'ready': return `v${u.version} is ready. It installs when you quit, or use Restart to update.`;
    case 'error': return `Update check failed: ${u.message}`;
    default: return u.status || 'Unknown update status.';
  }
}
