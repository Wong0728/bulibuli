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
    assert.match(live, /\/api\/live\/history\?limit=20/);
    assert.match(html, /live-health-summary/);
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
