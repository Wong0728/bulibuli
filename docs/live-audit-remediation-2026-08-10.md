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
