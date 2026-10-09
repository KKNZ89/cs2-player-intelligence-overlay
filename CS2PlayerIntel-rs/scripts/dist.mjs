// Builds the signed NSIS installer and writes it to release/ with latest.json for automatic updates.
// Installed copies read latest.json from the newest GitHub release, so its download link points at the
// release tagged v<version>.
//
// Updates must be signed. The private key is read from TAURI_SIGNING_PRIVATE_KEY or from
// %USERPROFILE%\.cs2intel\updater.key (create one with `npx tauri signer generate`, and put its public key
// in src-tauri/tauri.conf.json). Set CS2INTEL_RELEASE_BASE_URL to host the files somewhere else, and
// CS2INTEL_DEFAULT_UPDATE_SOURCE to build a copy that updates from another feed or folder (for testing).
import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO = 'KKNZ89/cs2-player-intelligence-overlay';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const release = path.join(root, 'release');
const { version } = JSON.parse(readFileSync(path.join(root, 'package.json'), 'utf8'));
const env = { ...process.env };

if (!env.TAURI_SIGNING_PRIVATE_KEY) {
  const keyFile = path.join(homedir(), '.cs2intel', 'updater.key');
  if (!existsSync(keyFile)) throw new Error(`No update signing key: set TAURI_SIGNING_PRIVATE_KEY or create ${keyFile}.`);
  env.TAURI_SIGNING_PRIVATE_KEY = readFileSync(keyFile, 'utf8').trim();
}
env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD ??= '';

const cli = path.join(root, 'node_modules', '@tauri-apps', 'cli', 'tauri.js');
const build = spawnSync(process.execPath, [cli, 'build', ...process.argv.slice(2)], { stdio: 'inherit', cwd: root, env });
if (build.status !== 0) process.exit(build.status ?? 1);

// CARGO_TARGET_DIR lets a build run while a copy started from the default target folder is still open.
const target = env.CARGO_TARGET_DIR ? path.resolve(root, 'src-tauri', env.CARGO_TARGET_DIR) : path.join(root, 'src-tauri', 'target');
const bundle = path.join(target, 'release', 'bundle', 'nsis');
const built = readdirSync(bundle).find(name => name.endsWith('-setup.exe') && name.includes(`_${version}_`));
if (!built || !existsSync(path.join(bundle, `${built}.sig`))) throw new Error(`The signed installer for ${version} was not found in ${bundle}.`);
// Release file names have no spaces (GitHub's download links would turn them into dots).
const installer = built.replaceAll(' ', '-');
mkdirSync(release, { recursive: true });
copyFileSync(path.join(bundle, built), path.join(release, installer));
copyFileSync(path.join(bundle, `${built}.sig`), path.join(release, `${installer}.sig`));
const base = (env.CS2INTEL_RELEASE_BASE_URL || `https://github.com/${REPO}/releases/download/v${version}`).replace(/\/$/, '');
const manifest = {
  version,
  notes: `CS2 Player Intel ${version}`,
  pub_date: new Date().toISOString(),
  platforms: {
    'windows-x86_64': {
      signature: readFileSync(path.join(bundle, `${built}.sig`), 'utf8').trim(),
      // A local release folder is served by the app itself, which keeps only the file name from this link.
      url: `${base}/${encodeURIComponent(installer)}`
    }
  }
};
writeFileSync(path.join(release, 'latest.json'), `${JSON.stringify(manifest, null, 2)}\n`);
console.log(`release/${installer}, its signature and latest.json are ready.`);
