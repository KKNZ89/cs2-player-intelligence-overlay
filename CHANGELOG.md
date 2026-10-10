# Changelog

## 1.0.4

- With a Steam Web API key, profiles and bans for the whole lobby come from two Steam requests instead of two per player, so Steam's limits are reached far less often and avatars (used to read the scoreboard) arrive sooner. CS2 hours still take one request per player.
- CSRep's security check can be completed once for everyone: when CSRep blocks lookups, the Match page shows a "Complete CSRep check" button. It opens one CSRep page; when you close it, every blocked player is looked up again.
- Diagnostics → Copy last match copies the log since your last match started, with repeated lines folded and one lookup line per player, short enough to paste into a message or an issue.

## 1.0.3

- Steam rate limits: when Steam keeps limiting profile requests, lookups pause for longer each time (10, 20, 40, then 60 minutes) instead of retrying every few minutes, and the log says so once. Steam profiles are reused for 12 hours.
- With a Steam Web API key, profiles come from Steam's API rather than the rate-limited community pages, and a rate limit on the community pages no longer pauses the API (each has its own pause).
- Scoreboard reading waits until players' Steam avatars are known, keeps its debug picture at most every 30 minutes, and shows the colour of every player (CS2 colours both teams).
- Optional CSRep sign-in (Settings → Data sources): signed-in visitors see CSRep's stats overview, so K/D, ADR, HLTV rating, KAST and time to damage come from CSRep too. Status shows Signed in / Not signed in, like CSStats. CSRep's "Sign in with Steam" pop-up now opens (the sign-in window allowed no pop-ups before).
- Your performance by map: each recorded match now also keeps your own kills, deaths, assists, MVPs and score from CS2, and the History page shows per map your matches, W–L–T, win rate, K/D, kills and MVPs per match, and a K/D trend over your last 20 matches there.
- Player details → Matches shows a By map table from the player's recent Leetify matches (works for you too): matches, W–L, win rate, average Leetify rating and its trend. It is shown live and never stored.
- Fixed CSRep and CSStats lookups failing with "a webview with label … already exists" right after their hidden window was closed.

## 1.0.2

- Automatic teams: with scoreboard reading on, everyone on CS2's scoreboard is put in your team or the other one while you hold Tab, so "Not assigned" goes away. Your own choice and spectating still win.
- The Match page says when CSStats or CSRep is switched off (the usual reason for N/A values), with a button to Settings → Data sources. N/A tooltips for a switched-off source say where to turn it on.

## 1.0.1

A maintenance release with no feature changes. Copies on 1.0.0 update to it by themselves: it confirms that automatic updates from GitHub releases install over the current version, without uninstalling, and keep your settings, history and notes.

## 1.0.0

First public release.

- Overlay next to CS2's scoreboard: compact view (hold Tab), expanded view (Ctrl+Tab), and a clickable view in CS2's Esc menu or with the mouse-navigation shortcut (Shift+F8).
- Player data from Leetify and Steam, plus FACEIT, CSStats and CSRep when you set them up. Every value shows its source and age; missing values explain why.
- Lobby summary, team averages, profile flags with reasons, and player details with match history and notes.
- Local match history (SQLite) with export, import and retention, and a History page listing your past matches and their players. Leetify data is never stored.
- Notes on players: as many as you like, each with date, time, match and whether they were with or against you, written from the dashboard, the overlay or the History page and shown again when you meet that player.
- Reading CSStats and CSRep pages is off by default and has to be turned on in Settings → Data sources.
- Optional teammate colours: with the setting on, the app reads each teammate's CS2 colour from the scoreboard (a capture of the CS2 window while Tab is held) and shows it as the avatar ring.
- API keys encrypted with Windows DPAPI; no passwords stored; no telemetry.
- Signed automatic updates from GitHub releases, installed in place by themselves when CS2 isn't running.
