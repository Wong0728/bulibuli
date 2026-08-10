/** Resolve the local recording action independently from the remote live label. */
export function getLiveActionState(room) {
    if (room?.is_recording) return 'stop';
    if (room?.can_start) return 'start';
    return 'disabled';
}

/** Merge gift bursts for display without changing authoritative archive events. */
export function mergeLiveEvents(events = []) {
    const merged = [];
    for (const event of events) {
        const previous = merged.at(-1);
        const sameGift = event.event_type === 'gift' && previous?.event_type === 'gift'
            && event.data?.uid === previous.data?.uid && event.data?.gift_name === previous.data?.gift_name;
        const freeGiftBucket = sameGift && event.data?.coin_type !== 'gold' && previous.data?.coin_type !== 'gold';
        const windowMs = freeGiftBucket ? 5000 : 2000;
        if (sameGift && (event.media_time_ms - previous.media_time_ms) <= windowMs) {
            previous.merged_count = (previous.merged_count || 1) + 1;
            previous.data = { ...previous.data, num: Number(previous.data?.num || 0) + Number(event.data?.num || 0) };
        } else merged.push({ ...event, data: { ...(event.data || {}) }, merged_count: 1 });
    }
    return merged;
}
