# CS2 Player Intel

A Windows companion for Counter-Strike 2 that discovers match players and displays Leetify, FACEIT, CSStats, CSRep, and Steam profile data in a configurable desktop overlay.

Built with Rust, Tauri 2, and a plain HTML/CSS/JavaScript interface. The active application lives in [CS2PlayerIntel-rs](CS2PlayerIntel-rs).

[Download releases](https://github.com/KKNZ89/cs2-player-intelligence-overlay/releases) · [Installation and full documentation](CS2PlayerIntel-rs/README.md) · [Report an issue](https://github.com/KKNZ89/cs2-player-intelligence-overlay/issues)

![CS2 Player Intel match dashboard with fictional sample players](CS2PlayerIntel-rs/docs/screenshots/dashboard.png)

| Compact overlay | Interactive player details |
|---|---|
| ![Compact side-panel overlay](CS2PlayerIntel-rs/docs/screenshots/overlay-side.png) | ![Clickable overlay in the CS2 Esc menu](CS2PlayerIntel-rs/docs/screenshots/overlay-esc-menu.png) |

Screenshots use fictional fixture data, not a live CS2 match.

## Features

- Compact, expanded, and interactive overlays with configurable columns, layout, and shortcuts.
- Provider-attributed statistics, missing-data explanations, team summaries, and player details.
- Local match history, notes, import/export, streaming privacy mode, and light/dark themes.
- Optional provider API keys protected with Windows DPAPI and signed automatic updates.
- No telemetry. Reading CSStats and CSRep pages is off until you turn it on.

## Requirements and quick start

Windows 10/11 x64, WebView2, Steam, and CS2. Use borderless or windowed CS2 for the overlay.

Download the installer from [Releases](https://github.com/KKNZ89/cs2-player-intelligence-overlay/releases), install the GSI configuration from Settings, and restart CS2. See the [full setup guide](CS2PlayerIntel-rs/README.md#install) for optional keys and provider sign-in.

For development, install Node.js 24, stable Rust/MSVC, Visual Studio C++ Build Tools, and the Windows SDK:

```powershell
Set-Location .\CS2PlayerIntel-rs
npm ci
npm start
```

## Safety and limitations

The application operates outside CS2: it does not inject code, read or write game memory, hook the game, or automate gameplay. **This does not guarantee anti-cheat approval or freedom from account restrictions.** Check the rules of your platform or competition and use at your own risk.

Profile flags describe statistical discrepancies, not proof of cheating. Provider data may be unavailable, private, rate-limited, or incomplete. The installer is not Authenticode-signed; updater signatures are a separate protection.

## Repository information

- [Application documentation and screenshot gallery](CS2PlayerIntel-rs/README.md)
- [Privacy: what is stored and sent](CS2PlayerIntel-rs/README.md#privacy)
- [Changelog](CHANGELOG.md)
- [Security policy: report vulnerabilities privately](SECURITY.md)
- [Windows verification workflow](.github/workflows/verify.yml)
- [MIT license](LICENSE)

This project is not affiliated with or endorsed by Valve or the data providers.
