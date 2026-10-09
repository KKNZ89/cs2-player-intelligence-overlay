// Starts the built app in smoke mode: hidden windows, a temporary data folder, and no Steam session, CS2
// launch, hotkeys, GSI server or provider lookups. Passes when both pages load and ask for state.
import { spawn, execFileSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

const exe = process.argv[2] || path.resolve('src-tauri/target/debug/cs2-player-intel.exe');
if (!existsSync(exe)) throw new Error(`Build the app first: ${exe} is missing.`);
const dir = mkdtempSync(path.join(tmpdir(), 'cs2intel-smoke-'));
const child = spawn(exe, [], { env: { ...process.env, CS2INTEL_SMOKE: dir }, stdio: 'inherit' });
const timer = setTimeout(() => {
  console.error('Smoke test timed out; stopping the app.');
  try { execFileSync('taskkill', ['/PID', String(child.pid), '/T', '/F']); } catch { /* Already gone. */ }
}, process.env.CS2INTEL_PROBE ? 180_000 : 30_000);
const code = await new Promise(resolve => child.on('exit', resolve));
clearTimeout(timer);
const log = existsSync(path.join(dir, 'diagnostics.log')) ? readFileSync(path.join(dir, 'diagnostics.log'), 'utf8') : '';
const failures = ['main', 'overlay'].filter(label => !log.includes(`state requested by ${label}`)).map(label => `the ${label} page never asked for state`);
if (code !== 0) failures.push(`exit code ${code}`);
for (const line of log.split('\n').filter(line => / ERROR /.test(line))) console.error(line);
// CS2INTEL_PROBE results (real provider lookups; run only while CS2 is closed).
for (const line of log.split('\n').filter(line => / INFO probe: /.test(line))) console.log(process.env.CS2INTEL_PROBE_TEXT ? line : line.slice(0, 2500));
// WebView2 may hold the profile briefly after exit.
setTimeout(() => { try { rmSync(dir, { recursive: true, force: true }); } catch { /* Left in the temp folder. */ } }, 2000);
if (failures.length) {
  console.error(`Smoke test failed: ${failures.join('; ')}.`);
  process.exitCode = 1;
} else console.log('Smoke test passed: dashboard and overlay loaded and received state.');
