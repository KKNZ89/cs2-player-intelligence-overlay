# Changelog

## 1.0.0

First public release.

- Overlay next to CS2's scoreboard: compact view (hold Tab), expanded view (Ctrl+Tab), and a clickable view in CS2's Esc menu or with the mouse-navigation shortcut (Shift+F8).
- Player data from Leetify and Steam, plus FACEIT, CSStats and CSRep when you set them up. Every value shows its source and age; missing values explain why.
- Lobby summary, team averages, profile flags with reasons, and player details with match history and notes.
- Local match history (SQLite) with export, import and retention, and a History page listing your past matches and their players. Leetify data is never stored.
- Reading CSStats and CSRep pages is off by default and has to be turned on in Settings → Data sources.
- Optional teammate colours: with the setting on, the app reads each teammate's CS2 colour from the scoreboard (a capture of the CS2 window while Tab is held) and shows it as the avatar ring.
- API keys encrypted with Windows DPAPI; no passwords stored; no telemetry.
- Signed automatic updates from GitHub releases, installed in place by themselves when CS2 isn't running.
