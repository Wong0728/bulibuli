import { apiGet } from './core.js';

let statusInFlight = null;
let healthInFlight = null;
let lastStatus = null;
let lastStatusAt = 0;
let lastHealth = null;
let lastHealthAt = 0;

async function fetchShared(url, kind, maxAgeMs) {
    const now = Date.now();
    const cached = kind === 'status' ? lastStatus : lastHealth;
    const cachedAt = kind === 'status' ? lastStatusAt : lastHealthAt;
    if (cached && now - cachedAt <= maxAgeMs) return cached;

    const current = kind === 'status' ? statusInFlight : healthInFlight;
    if (current) return current;
    const request = apiGet(url)
        .then(result => {
            if (kind === 'status') {
                lastStatus = result;
                lastStatusAt = Date.now();
            } else {
                lastHealth = result;
                lastHealthAt = Date.now();
            }
            return result;
        })
        .finally(() => {
            if (kind === 'status') statusInFlight = null;
            else healthInFlight = null;
        });
    if (kind === 'status') statusInFlight = request;
    else healthInFlight = request;
    return request;
}

export function fetchDownloadSnapshot(maxAgeMs = 250) {
    return fetchShared('/api/download/status', 'status', maxAgeMs);
}

export function fetchDownloadHealth(maxAgeMs = 2000) {
    return fetchShared('/api/download/health', 'health', maxAgeMs);
}
