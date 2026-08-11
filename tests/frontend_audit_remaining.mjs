import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const read = file => fs.readFileSync(path.join(root, file), 'utf8');

test('settings fragment exposes the four requested groups', () => {
    const html = read('static/settings.html');
    for (const group of ['basic', 'downloads', 'advanced', 'security']) {
        assert.match(html, new RegExp(`data-settings-group="${group}"`));
    }
    assert.match(read('static/js/settings.js'), /selectGroup\('basic'\)/);
});

test('blogger add and edit use one mode-driven modal', () => {
    const html = read('static/index.html');
    assert.match(html, /id="blogger-modal"/);
    assert.doesNotMatch(html, /id="(?:add|edit)-blogger-modal"/);
    const modal = read('static/js/modal.js');
    assert.match(modal, /configureBloggerModal\('add'\)/);
    assert.match(modal, /configureBloggerModal\('edit'\)/);
    assert.match(modal, /getBloggerModalMode\(\) === 'edit'/);
});

test('state proxy notifies subscribers and supports patch updates', async () => {
    const { _state, subscribeState, updateState } = await import('../static/js/state.js');
    const updates = [];
    const unsubscribe = subscribeState('auditTest', (value, previous) => updates.push([value, previous]));
    updateState({ auditTest: 1 });
    _state.auditTest = 2;
    unsubscribe();
    _state.auditTest = 3;
    assert.deepEqual(updates, [[1, undefined], [2, 1]]);
});

test('active-tab polling pauses hidden pages and routes history/live work', () => {
    const source = read('static/js/polling.js');
    assert.match(source, /document\.hidden/);
    assert.match(source, /_state\.currentTab === 'history'/);
    assert.match(source, /_state\.currentTab === 'live'/);
    assert.match(source, /setTimeout\(poll, ACTIVE_TAB_POLL_MS\)/);
});

test('cookie status distinguishes transient states from expiration', () => {
    const source = read('static/js/auth-card.js');
    assert.match(source, /risk_control/);
    assert.match(source, /unreachable/);
    assert.match(source, /malformed/);
    assert.match(source, /不会误判为过期/);
});

test('portable build has a development-only esbuild dependency', () => {
    const packageJson = JSON.parse(read('static/js/package.json'));
    assert.ok(packageJson.devDependencies.esbuild);
    assert.match(read('build.py'), /npm.*run.*build/);
});
