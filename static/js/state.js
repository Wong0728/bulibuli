// 全局统一的网络错误文案：右上角 toast 与顶栏横幅共用，保证各处提示一致。
// 放在 state.js（无任何依赖的叶子模块）中导出，供各业务模块安全引用。
export const _NETWORK_ERR_MSG = '网络连接异常，请检查网络或后端服务状态';

export const _state = {
    socket: null,
    isWebSocketConnected: false,
    wsConnected: false,
    seenWebSocketMessages: new Set(),
    elementHandlers: new WeakMap(),
    currentControllers: [],
    errorMsgSet: new Map(),
    boardRefreshTimer: null, // 看板刷新防抖定时器
    burningTasks: new Map(),
    boardCardIndex: new Map(),
    downloadPollTimer: null,
    urlExpiryInterval: null,
    logRefreshInFlight: false,
    statusPollingInFlight: false,
    qrcodePollInFlight: false,
    boardPullInFlight: false,
    sessionRole: 'owner',
};

export function webSocketMessageKey(namespace, message) {
    if (!namespace || !message?.id) return null;
    return `${namespace}:${message.id}`;
}
