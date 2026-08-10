import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { ApiError, parseEnvelope } from '../static/js/api.js';
import { getQRCodePayload, getQRCodePollState } from '../static/js/qrcode-contract.js';
import { getLiveActionState, mergeLiveEvents } from '../static/js/live-contract.js';

test('accepts the canonical API envelope without flattening data', () => {
    const envelope = parseEnvelope({
        code: 0,
        message: 'success',
        data: { statuses: { task: { status: 'completed' } } },
    });

    assert.deepEqual(envelope.data.statuses.task.status, 'completed');
    assert.equal(Object.hasOwn(envelope, 'statuses'), false);
});

test('rejects responses that omit the data member', () => {
    assert.throws(
        () => parseEnvelope({ code: 0, message: 'success' }),
        error => error instanceof ApiError && error.code === 502,
    );
});

test('turns non-zero API codes into typed errors while retaining data', () => {
    assert.throws(
        () => parseEnvelope({ code: 403, message: 'forbidden', data: { reason: 'policy' } }, 403),
        error => error instanceof ApiError
            && error.code === 403
            && error.status === 403
            && error.data.reason === 'policy',
    );
});

test('reads QR generation fields only from the canonical data payload', () => {
    const envelope = parseEnvelope({
        code: 0,
        message: 'success',
        data: { url: 'https://example.test/qr', qrcode_key: 'key-123' },
    });

    assert.deepEqual(getQRCodePayload(envelope), {
        url: 'https://example.test/qr',
        qrcodeKey: 'key-123',
    });
    assert.equal(getQRCodePayload({ ...envelope, url: 'wrong', qrcode_key: 'wrong', data: {} }), null);
});

test('rejects QR generation payloads with a missing required field', () => {
    assert.equal(getQRCodePayload({ code: 0, message: 'success', data: { url: 'https://example.test/qr' } }), null);
    assert.equal(getQRCodePayload({ code: 0, message: 'success', data: { qrcode_key: 'key-123' } }), null);
});

test('maps QR poll business codes from the data payload', () => {
    const cases = [
        [86101, 'waiting'],
        [86090, 'scanned'],
        [86038, 'expired'],
        [0, 'success'],
        [-1, 'unexpected'],
    ];

    for (const [code, kind] of cases) {
        assert.deepEqual(getQRCodePollState({
            code: 0,
            message: 'success',
            data: { code, message: `status ${code}` },
        }), { kind, code, message: `status ${code}` });
    }
    assert.deepEqual(getQRCodePollState({ code: 0, message: 'success', data: {} }), {
        kind: 'invalid',
        code: null,
        message: '二维码轮询响应缺少状态码',
    });
});

test('settings saves retain the response payload rather than outer envelope fields', () => {
    const envelope = parseEnvelope({
        code: 0,
        message: '设置已保存',
        data: { current: { query: { manual_query_limit: 20 } } },
    });
    const savedSnapshot = structuredClone(envelope.data);

    assert.deepEqual(savedSnapshot, { current: { query: { manual_query_limit: 20 } } });
    assert.equal(Object.hasOwn(envelope, 'current'), false);
});

test('live controls separate remote live state from local recording state', () => {
    assert.equal(getLiveActionState({ is_recording: true, can_start: false }), 'stop');
    assert.equal(getLiveActionState({ is_recording: false, can_start: true }), 'start');
    assert.equal(getLiveActionState({ is_recording: false, can_start: false }), 'disabled');
    // Local recording remains stoppable even when the anchor has just gone offline.
    assert.equal(
        getLiveActionState({ live_status: 0, is_recording: true, can_start: false }),
        'stop',
    );
});

test('live gift bursts merge only inside their display windows', () => {
    const base = { event_type: 'gift', data: { uid: 7, gift_name: '小花', num: 1, coin_type: 'silver' } };
    const merged = mergeLiveEvents([
        { ...base, seq: 1, media_time_ms: 1000 },
        { ...base, seq: 2, media_time_ms: 5500 },
        { ...base, seq: 3, media_time_ms: 11000 },
    ]);
    assert.equal(merged.length, 2);
    assert.equal(merged[0].data.num, 2);
    assert.equal(merged[0].merged_count, 2);
    assert.equal(merged[1].merged_count, 1);
});

test('paid gift display merge uses the shorter two second window', () => {
    const event = { event_type: 'gift', data: { uid: 8, gift_name: '醒目礼物', num: 1, coin_type: 'gold' } };
    assert.equal(mergeLiveEvents([
        { ...event, seq: 1, media_time_ms: 0 },
        { ...event, seq: 2, media_time_ms: 2500 },
    ]).length, 2);
});

test('frontend uses the canonical video info route', () => {
    const frontendSources = [
        readFileSync(new URL('../static/js/manual.js', import.meta.url), 'utf8'),
        readFileSync(new URL('../static/js/media-actions.js', import.meta.url), 'utf8'),
    ];
    const backendRoutes = readFileSync(new URL('../src/api/video.rs', import.meta.url), 'utf8');

    for (const source of frontendSources) {
        assert.equal(source.includes('/api/video/get-video-info'), false);
    }
    assert.equal(frontendSources.every(source => source.includes('/api/video/info?bvid=')), true);
    assert.equal(backendRoutes.includes('"/api/video/info"'), true);
});

test('live UI defaults saved sources to manual-only and exposes trustworthy status regions', () => {
    const live = readFileSync(new URL('../static/js/live.js', import.meta.url), 'utf8');
    const html = readFileSync(new URL('../static/index.html', import.meta.url), 'utf8');
    assert.match(live, /auto_record_enabled: false/);
    assert.match(live, /pendingRooms/);
    assert.match(live, /\/api\/live\/history\?limit=30/);
    // 页面连接 / 监控 worker / B 站新鲜度三态独立展示，任一失败不清空整页
    assert.match(html, /live-sync-page/);
    assert.match(html, /live-sync-monitor/);
    assert.match(html, /live-sync-bili/);
    assert.match(html, /live-history-list/);
});

test('live UI exposes bounded polling, schedule validation, and cancellable merge jobs', () => {
    const live = readFileSync(new URL('../static/js/live.js', import.meta.url), 'utf8');
    const backend = readFileSync(new URL('../src/api/live.rs', import.meta.url), 'utf8');
    assert.match(live, /dashboardInFlight/);
    assert.match(live, /eventsInFlight/);
    assert.match(live, /visibilitychange/);
    assert.match(live, /validateScheduleStrict/);
    assert.match(live, /merge-cancel/);
    assert.match(backend, /\/api\/live\/merge\/{job_id}\/cancel/);
    assert.match(backend, /server_timezone/);
});

test('live UI maps internal enum values to Chinese and degrades gracefully on failure', () => {
    const live = readFileSync(new URL('../static/js/live.js', import.meta.url), 'utf8');
    // 英文内部状态一律映射中文展示
    assert.match(live, /interactionStateText/);
    assert.match(live, /captureModeText/);
    assert.match(live, /stopReasonText/);
    // dashboard 失败后保留旧数据并标记，不再整页清空
    assert.match(live, /dashboardFailedAt/);
    // 离开直播 Tab 时停止轮询
    assert.match(live, /liveTabActive/);
    // 停止/删除等危险操作走统一确认弹窗而非 window.confirm
    assert.match(live, /confirmDialog/);
    assert.doesNotMatch(live, /window\.confirm/);
});

test('live sources expose per-room quality cap wired to settings and recorder', () => {
    const live = readFileSync(new URL('../static/js/live.js', import.meta.url), 'utf8');
    const html = readFileSync(new URL('../static/index.html', import.meta.url), 'utf8');
    const backend = readFileSync(new URL('../src/api/live.rs', import.meta.url), 'utf8');
    const recorder = readFileSync(new URL('../src/services/live_recorder/mod.rs', import.meta.url), 'utf8');
    // 设置弹窗提供清晰度上限并随 update 保存
    assert.match(html, /live-source-quality/);
    assert.match(live, /max_qn/);
    assert.match(backend, /max_qn/);
    // 录制器把每源清晰度上限传给流地址请求
    assert.match(recorder, /source_max_qn/);
    // 全局直播设置（并发/磁盘/时长/文件名模板）可从设置页配置
    assert.match(html, /setting-live-max-concurrent/);
    assert.match(html, /setting-live-file-template/);
    assert.match(recorder, /render_file_template/);
});

test('live recordings expose danmaku burn-in reusing the download burn pipeline', () => {
    const live = readFileSync(new URL('../static/js/live.js', import.meta.url), 'utf8');
    const backend = readFileSync(new URL('../src/api/live.rs', import.meta.url), 'utf8');
    const burner = readFileSync(new URL('../src/services/subtitle_burner/burn.rs', import.meta.url), 'utf8');
    // 历史条目展示烧录入口与已有弹幕版标记
    assert.match(live, /has_burned/);
    assert.match(live, /history-burn/);
    // 前端轮询复用下载烧录的状态接口
    assert.match(live, /\/api\/download\/burn\/status\//);
    // 后端新路由接入共享烧录任务队列
    assert.match(backend, /burn-danmaku/);
    assert.match(backend, /burn_tasks/);
    // 烧录器提供直接接收互动条目的入口，跳过 BV 号查找
    assert.match(burner, /burn_live_interactions/);
});
