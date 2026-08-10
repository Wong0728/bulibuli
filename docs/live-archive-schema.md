# Live archive schema and recovery contract

This document describes the live recording artifacts introduced by the
reliability remediation. JSONL is the only event format written synchronously
while a session is running. Legacy JSON and XML are generated once, after the
JSONL writer drains its tail, so one event is not synchronously written three
times.

## Files

| Artifact | Purpose | Lifecycle |
| --- | --- | --- |
| `*_events.jsonl` | Canonical append-only event stream | Written during capture; retained as the source of truth |
| `*_danmu.json` | Legacy-compatible archive | Generated after capture with `schema_version: 2` |
| `*_danmaku.xml` | Custom/compatibility archive | Generated after capture with `schema_version="2"` |
| `*_summary.json` | Session statistics and recovery markers | Checkpointed during capture and finalized after capture |
| `*_segment_N.flv` | Recoverable media segment | Retained until a successful merge and ffprobe validation |

Every archived event includes its event type, media time, segment index, and
source payload. User identifiers in XML are replaced by `redacted`; the
canonical JSONL and legacy JSON remain protected server-side and are not
serialized into viewer-facing dashboard responses.

## Summary fields

`capture_gaps` is a bounded list of gap markers with the sequence/time window,
reason, and dropped count. `heat_buckets` stores one-minute buckets (up to 24
hours), `paid_markers` and `link_markers` are bounded event marker lists, and
`unique_user_count` is calculated from a bounded user set. A bounded guard
dedupe set prevents repeated SC/guard events from growing memory without
limit. Estimates use paid gold-coin events only; free gifts are excluded.

## Recovery and merge

The recorder persists recording checkpoints and exposes safe recovery metadata:

* `GET /api/live/recovery` lists recoverable recordings and segment counts.
* `POST /api/live/history/{recording_id}/merge` starts a retryable merge job.
* `GET /api/live/merge/{job_id}` returns queued/running/completed/failed state,
  progress, and a redacted error.
* `POST /api/live/merge/{job_id}/cancel` cancels a queued/running merge and
  leaves every source segment available for another retry.

Merge writes a temporary `.mp4.partial`, applies a timeout, validates at least
one media track and a positive duration with ffprobe, and atomically renames
the result only after validation. Source segments are never deleted on
failure. A successful merge is the only condition that permits cleanup.

## Safety limits

The default protection limits are two concurrent recordings, 10 GiB minimum
free disk space, 12 hours maximum duration, 200 GiB maximum recording size,
20,000 dedupe keys, and bounded archive marker/user sets. These are deliberate
safe defaults; future configuration changes must preserve finite upper bounds.

Absolute paths are internal implementation data. `RecordingInfo` omits media,
event, XML, and summary paths from JSON serialization; APIs expose recording
IDs, status, progress, and recovery metadata instead.
