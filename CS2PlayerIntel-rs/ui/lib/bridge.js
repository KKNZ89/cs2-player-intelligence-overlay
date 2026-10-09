// The UI's only connection to the Rust backend: Tauri commands (invoke) and events (listen).
// Pages use this API and never touch Tauri directly, so the boundary stays in one file.

const tauri = window.__TAURI__;
if (!tauri) throw new Error('CS2 Player Intel must run inside its desktop app.');
const { invoke } = tauri.core;
const { listen } = tauri.event;

export const api = {
  // Calls back with the current state now and on every change; returns an unsubscribe function.
  onState(callback) {
    let stop = null;
    let stopped = false;
    listen('state:update', event => callback(event.payload)).then(unlisten => {
      if (stopped) unlisten(); else stop = unlisten;
    });
    invoke('state_get').then(callback);
    return () => { stopped = true; stop?.(); };
  },
  onCommand(callback) {
    listen('ui:command', event => callback(event.payload));
  },
  saveSettings: input => invoke('settings_save', { input }),
  pickPath: kind => invoke('path_pick', { kind }),
  installGsi: () => invoke('gsi_install'),
  openCsStatsLogin: () => invoke('csstats_login'),
  openCsRepLogin: () => invoke('csrep_login'),
  openProfile: (provider, steamId) => invoke('profile_open', { provider, steamId }),
  openLink: target => invoke('link_open', { target }),
  addSteamId: steamId => invoke('player_add', { steamId }),
  selectPlayers: input => invoke('players_select', { input }),
  useAutomaticRoster: () => invoke('players_automatic'),
  removePlayer: steamId => invoke('player_remove', { steamId }),
  setPlayerSide: (steamId, side) => invoke('player_side', { steamId, side }),
  refresh: () => invoke('players_refresh'),
  refreshPlayer: steamId => invoke('player_refresh', { steamId }),
  toggleOverlay: () => invoke('overlay_toggle'),
  endOverlayInteraction: () => invoke('overlay_interaction_end'),
  fitOverlay: (height, sideWidth) => invoke('overlay_fit', { height, sideWidth }),
  checkForUpdates: () => invoke('update_check'),
  installUpdate: () => invoke('update_install'),
  launchGame: () => invoke('game_launch'),
  recentDiagnostics: () => invoke('diagnostics_recent'),
  playerHistory: steamId => invoke('player_history', { steamId }),
  addNote: (steamId, text, matchId = null) => invoke('note_add', { steamId, text, matchId }),
  deleteNote: id => invoke('note_delete', { id }),
  historyStats: () => invoke('history_stats'),
  exportHistory: () => invoke('history_export'),
  clearHistory: notes => invoke('history_clear', { notes }),
  historyMatches: (limit, offset) => invoke('history_matches', { limit, offset }),
  historyMatch: id => invoke('history_match', { id }),
  historyPerformance: () => invoke('history_performance'),
  importHistory: () => invoke('history_import'),
  openDiagnosticsFolder: () => invoke('diagnostics_open')
};
