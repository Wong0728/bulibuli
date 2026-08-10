# Live audit remediation record — 2026-08-10

This record maps the latest live audit scope to the implemented changes. It is
an implementation checklist, not a replacement for the audit report.

| Area | Remediation status | Evidence |
| --- | --- | --- |
| LIVE-BILI / LIVE-DM | Complete | WBI-only `getDanmuInfo`, authenticated WebSocket handshake, bounded retry/circuit-breaker, host rotation, stale/risk states |
| LIVE-REC | Complete | session cancellation, start/stop compensation, background stop/merge job, FFmpeg reconnect/timeout, duration/size/disk guards |
| LIVE-DATA | Complete | JSONL canonical archive, post-session legacy generation, bounded markers/users, capture gaps, checkpoints, recovery API |
| LIVE-API | Complete | safe dashboard serialization, operation IDs, merge/recovery endpoints, redacted diagnostics |
| LIVE-SCH | Complete | new-source auto-record off, normalized/overlap-checked windows, server timezone and next-check metadata |
| LIVE-UX / LIVE-CSS / LIVE-FE | Complete | stable keyed updates, visibility/in-flight polling guards, status/stale/error labels, structured schedule editor, keyboard/ARIA/mobile/dark-theme support |
| LIVE-TST | Complete | 193 Rust tests, WebSocket auth/reject/retry tests, real FFmpeg/ffprobe merge test, frontend contract tests, format and Clippy gates |

## Compatibility and rollout notes

Existing source auto-record settings are not migrated. New sources default to
manual-only. Existing historical `getConf` documentation remains marked as
research-only; production code has no automatic fallback. The default schedule
policy remains “record until stream end”; background stop returns an operation
ID and can be observed or retried. Original segments are retained unless merge
and ffprobe validation both succeed.

Before production rollout, perform one low-frequency manual verification with
an authorized account: WBI `getDanmuInfo`, WebSocket auth, a short recording,
stop/merge, and recovery from retained segments. Do not use high-frequency
automated requests against Bilibili.

## Page rebuild and feature integration — 2026-08-10 (second pass)

| Area | Change | Evidence |
| --- | --- | --- |
| Backoff decay (LIVE-BILI-077) | Decay no longer arms a new backoff window after successful batches; 3 successful batches lower one level, level 0 clears it | `LiveMonitor::note_successful_batch` |
| CDN rotation (LIVE-BILI-084/085) | Segment refresh prefers candidates with the same container/codec as the active session before crossing formats | `RecordingWorker::refresh_segment` |
| Merge resources (LIVE-DATA-104) | Merge pre-checks free space (segments + 1 GiB margin) and keeps source segments on any failure | `merge_segments_to_mp4_inner` |
| Dashboard contract | Dashboard returns disk availability (bytes, no path); public errors keep URL host but strip signed tokens | `api::live::dashboard`, `public_error` |
| Page rebuild (LIVE-UX-021+) | Live tab rebuilt on the blogger dashboard (sidebar + detail) and download board (sub-tabs) design language; page/monitor/B-site freshness shown as three independent states; failure keeps old data with stale markers | `static/index.html#tab-live`, `static/js/live.js` |
| Schedule visibility | Weekly schedule editor (7 days × 2 windows, time pickers, per-window clear, live validation, server timezone note) lives in the standard modal; detail panel summarizes policy and next run | `live-source-modal`, `live-strategy-summary` |
| Feature parity (BililiveRecorder/blrec/biliup) | Per-source quality cap (`max_qn`, migration 009), configurable concurrency/disk/duration limits and file name template (`RuntimeSettings.live`), bulk room import, history rows expose stop reason/segments/restarts | `m20260810_000009`, `LiveRecordingSettings`, `live-add-modal` |

Not migrated in this pass (recorded as future work, see audit section 10):
automatic time/size-based segment slicing, a standalone log viewer, and a
danmaku XML player. Recording-path copy is intentionally not exposed; error
messages and dashboard payloads never return absolute paths.
