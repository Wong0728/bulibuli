import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const read = file => fs.readFileSync(path.join(root, file), 'utf8');

test('live source settings expose explicit all-day and weekly schedule modes', () => {
    const html = read('static/index.html');
    const live = read('static/js/live.js');
    assert.match(html, /name="live-source-schedule-mode" value="all-day"/);
    assert.match(html, /name="live-source-schedule-mode" value="weekly"/);
    assert.doesNotMatch(html, /id="live-source-all-day"/);
    assert.match(live, /weekly_schedule: finalAllDay \? null : schedule/);
    assert.match(live, /clear_schedule: finalAllDay/);
    assert.match(live, /setScheduleMode\(source\.schedule_all_day \? 'all-day' : 'weekly'\)/);
});

test('drawer keeps sidecar viewing in the lower browser and filters it from artifacts', () => {
    const drawer = read('static/js/drawer-render.js');
    assert.match(drawer, /primaryArtifactFiles\(files\)/);
    assert.match(drawer, /!\['danmaku', 'comment'\]\.includes/);
    assert.doesNotMatch(drawer, /if \(type === 'comment'\)/);
    assert.doesNotMatch(drawer, /else if \(type === 'danmaku'\)/);
    assert.match(drawer, /renderSidecarBrowser\(video\.files, bvid\)/);
});

test('directory actions are capability-gated in every board renderer', () => {
    const history = read('static/js/history.js');
    const drawer = read('static/js/drawer-render.js');
    const live = read('static/js/live.js');
    const boardApi = read('src/api/history/board.rs');
    const historyApi = read('src/api/history/crud.rs');

    assert.match(history, /v\.can_open_directory && v\.relative_path/);
    assert.match(drawer, /canOpenDirectory && video\.relative_path/);
    assert.match(drawer, /canOpenDirectory && f\.path/);
    assert.match(live, /liveState\.dashboard\?\.can_open_directory && item\.has_output/);
    assert.match(boardApi, /"can_open_directory": can_open_directory/);
    assert.match(historyApi, /仅本机访问支持打开所在目录/);
});
