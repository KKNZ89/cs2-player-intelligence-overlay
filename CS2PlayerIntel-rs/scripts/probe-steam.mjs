// Checks the Steam helper against the real Steam client: starts it, reads the co-play list once and stops.
//   npm run probe:steam [path\to\steam_api64.dll]
// While the helper runs, Steam counts it as CS2. Run this when you are not about to launch or play CS2.
import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import path from 'node:path';
import readline from 'node:readline';

const exe = path.resolve('src-tauri/target/debug/cs2-player-intel.exe');
const dll = process.argv[2] || 'C:\Program Files (x86)\Steam\steamapps\common\Counter-Strike Global Offensive\game\bin\win64\steam_api64.dll';
if (!existsSync(exe)) throw new Error(`Build the app first (cargo build): ${exe} is missing.`);
if (!existsSync(dll)) throw new Error(`steam_api64.dll not found at ${dll}; pass its path.`);

const helper = spawn(exe, ['--steam-helper'], { env: { ...process.env, SteamAppId: '730' }, stdio: ['pipe', 'pipe', 'inherit'] });
const lines = readline.createInterface({ input: helper.stdout })[Symbol.asyncIterator]();
async function request(message) {
  helper.stdin.write(`${JSON.stringify(message)}\n`);
  const { value, done } = await lines.next();
  if (done) throw new Error('The helper exited.');
  const response = JSON.parse(value);
  if (!response.ok) throw new Error(response.error);
  return response;
}

try {
  await request({ type: 'init', file: dll });
  const since = Math.floor(Date.now() / 1000) - 24 * 3600;
  const { entries, friends } = await request({ type: 'snapshot', since });
  const cs2 = entries.filter(entry => entry.gameId === 730 && entry.time >= since);
  console.log(`Steam helper works: ${entries.length} co-play entries, ${cs2.length} from CS2 in the last 24 hours, ${friends.length} friends in CS2.`);
} catch (error) {
  console.error(`Steam helper failed: ${error.message}`);
  process.exitCode = 1;
} finally {
  helper.stdin.write('{"type":"shutdown"}\n');
  helper.stdin.end();
}
