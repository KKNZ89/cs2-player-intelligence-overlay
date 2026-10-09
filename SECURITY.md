# Security policy

## Reporting a vulnerability

Please don't open a public issue for security problems. Use GitHub's private reporting instead: **Security → Report a vulnerability** on this repository.

Include what you found, how to reproduce it, and which version you tested. You should get a reply within a week. Fixes ship as a signed update through the normal update feed.

## Supported versions

Only the latest release gets fixes. Installed copies update themselves, so staying on the latest version needs no action.

## What counts

Examples of things worth reporting:

- A provider page or any website reaching the app's commands or local files.
- A way to install an update that isn't signed with the project's key.
- API keys, GSI tokens or other secrets leaking into logs, exports or network requests.
- The local GSI server accepting posts without the per-install token.

Out of scope: problems in CS2, Steam or the data providers themselves, and the installer lacking an Authenticode signature (known, see the README).
