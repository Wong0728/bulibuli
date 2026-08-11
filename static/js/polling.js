import { _state } from './state.js';
import { loadHistoryBoard } from './history.js';
import { updateDownloadLists } from './download-queue.js';

const ACTIVE_TAB_POLL_MS = 10000;

export function startPollingScheduler() {
    if (_state.activeTabPollTimer) clearTimeout(_state.activeTabPollTimer);
    const poll = async () => {
        if (!document.hidden) {
            if (_state.currentTab === 'history' && !_state.boardRefreshInFlight) {
                _state.boardRefreshInFlight = true;
                try {
                    await loadHistoryBoard(_state.currentBoardTab);
                    await updateDownloadLists();
                } finally {
                    _state.boardRefreshInFlight = false;
                }
            } else if (_state.currentTab === 'live') {
                window.refreshDashboard?.(true);
            }
        }
        _state.activeTabPollTimer = setTimeout(poll, ACTIVE_TAB_POLL_MS);
    };
    poll();
}
