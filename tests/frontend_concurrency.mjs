import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const root = new URL('../', import.meta.url);
const source = async name => readFile(new URL(`static/js/${name}`, root), 'utf8');

test('API wrappers accept caller request options', async () => {
  const core = await source('core.js');
  assert.match(core, /export async function apiGet\(url, options = \{\}\)/);
  assert.match(core, /signal: callerSignal \|\| controller\.signal/);
});

test('history and drawer reject stale responses', async () => {
  const history = await source('history.js');
  const drawer = await source('drawer.js');
  assert.match(history, /historyBoardRequestId/);
  assert.match(history, /historyBoardController\?\.abort/);
  assert.match(drawer, /drawerRequestId/);
  assert.match(drawer, /currentDrawerBvid !== bvid/);
});

test('manual pagination and live recording identity are explicit', async () => {
  const manual = await source('manual.js');
  const live = await source('live.js');
  const settings = await source('settings.js');
  assert.match(manual, /offset: 0/);
  assert.match(manual, /data\.has_more/);
  assert.match(live, /recording_id=/);
  assert.match(live, /mergePending/);
  assert.match(settings, /settings\.expected_revision = settings\.revision/);
});
