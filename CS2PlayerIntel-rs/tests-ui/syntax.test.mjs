import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

// The page scripts run only inside the app, so a syntax error would otherwise show up first in the smoke test.
const ui = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', 'ui');
const scripts = readdirSync(ui, { recursive: true }).filter(file => String(file).endsWith('.js')).map(file => path.join(ui, String(file)));

test('every UI script parses', () => {
  assert.ok(scripts.length >= 5, 'the UI scripts were found');
  for (const file of scripts) {
    const check = spawnSync(process.execPath, ['--check', file], { encoding: 'utf8' });
    assert.equal(check.status, 0, `${path.relative(ui, file)}: ${check.stderr}`);
  }
});
