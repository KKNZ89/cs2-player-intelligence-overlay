// Your own live stats from GSI. Other players' live stats are not available and never estimated.

const MESSAGES = {
  disconnected: 'Waiting for CS2. Install GSI in Settings, then restart CS2.',
  'not-in-match': 'Join a match to see your live stats.',
  'observing-other-player': 'Spectating: your own stats appear after your next life.',
  waiting: 'Waiting for player match data from CS2.'
};

export function renderLiveStats(element, gsi, { tiles = false } = {}) {
  // Identical updates are skipped: rebuilding costs layout work in a window drawn over the game.
  const key = JSON.stringify([tiles, gsi.matchStatsStatus, gsi.kills, gsi.deaths, gsi.assists, gsi.kd, gsi.mvps, gsi.score]);
  if (element.dataset.key === key) return;
  element.dataset.key = key;
  const available = gsi.matchStatsStatus === 'available' || gsi.matchStatsStatus === 'last-known';
  element.classList.toggle('available', available);
  element.classList.toggle('tiles', tiles && available);
  if (!available) {
    element.textContent = MESSAGES[gsi.matchStatsStatus] || MESSAGES.waiting;
    return;
  }
  const value = number => (Number.isFinite(number) ? String(number) : '—');
  const kd = Number.isFinite(gsi.kd) ? gsi.kd.toFixed(2) : '—';
  const stats = [['Kills', value(gsi.kills)], ['Deaths', value(gsi.deaths)], ['Assists', value(gsi.assists)], ['K/D', kd], ['MVPs', value(gsi.mvps)], ['Score', value(gsi.score)]];
  const lastKnown = gsi.matchStatsStatus === 'last-known' ? 'Last known (spectating)' : '';
  if (!tiles) {
    element.textContent = `${lastKnown ? `${lastKnown} · ` : 'YOU · '}${stats.map(([label, text]) => `${label} ${text}`).join(' · ')}`;
    return;
  }
  element.replaceChildren(...stats.map(([label, text]) => {
    const tile = document.createElement('span');
    tile.className = 'stat';
    const name = document.createElement('span');
    name.className = 'label';
    name.textContent = label;
    const number = document.createElement('b');
    number.textContent = text;
    tile.append(name, ' ', number);
    return tile;
  }));
  if (lastKnown) {
    const note = document.createElement('small');
    note.className = 'stale-note';
    note.textContent = lastKnown;
    element.append(note);
  }
}
