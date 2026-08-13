// 1. 生成背景碎片（位置与外观由 pair.css 的 .floating-bit 规则控制）
const stage = document.getElementById('stage');
const bits = [
    'BVID', 'UID', 'API:OK', 'GET /video', 'WS:SYNC', 'CLOUD',
    '1080P', 'BV1xx...', 'RE-UP', 'TASK:START', 'DB:SAVE',
    'aria2', 'ffmpeg', 'DANMAKU', 'MONITOR', 'COOKIE:OK',
    'SESSION', 'ARCHIVE', 'AV->BV', 'HEVC', 'SUB:BURN', 'RETRY:3',
];

const elements = [];
const layerCount = 28;
const reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

for (let i = 0; i < layerCount; i++) {
    const el = document.createElement('div');
    el.className = 'floating-bit';
    el.innerText = bits[i % bits.length];

    // 随机视差深度 (0.02 ~ 0.1)；用 WAAPI 持久动画承载位移，避免内联样式（CSP）
    const animation = el.animate(
        [{ transform: 'translate(0px, 0px)' }, { transform: 'translate(0px, 0px)' }],
        { duration: 1000, fill: 'both' },
    );
    animation.pause();

    elements.push({
        dom: el,
        depth: 0.02 + Math.random() * 0.08,
        x: 0,
        y: 0,
        animation,
    });
    stage.appendChild(el);
}

// 2. 指针移动视差效果（rAF 平滑跟随）
let pointerX = 0;
let pointerY = 0;

window.addEventListener('mousemove', (e) => {
    pointerX = e.clientX - window.innerWidth / 2;
    pointerY = e.clientY - window.innerHeight / 2;
}, { passive: true });

function parallaxTick() {
    elements.forEach(item => {
        item.x += (pointerX * item.depth - item.x) * 0.12;
        item.y += (pointerY * item.depth - item.y) * 0.12;
        const transform = `translate(${item.x.toFixed(2)}px, ${item.y.toFixed(2)}px)`;
        item.animation.effect.setKeyframes([{ transform }, { transform }]);
    });
    requestAnimationFrame(parallaxTick);
}

if (!reduceMotion) {
    requestAnimationFrame(parallaxTick);
}

// 3. 输入处理
const input = document.getElementById('pairing-input');
const form = document.getElementById('pairing-form');
const btn = document.getElementById('submit-btn');
const btnText = btn.querySelector('.btn-text');
const hint = document.getElementById('hint');
const card = document.getElementById('card');

let paired = false;
let pairingExpiresAt = null;
let stateLoadInFlight = false;

function setHint(text, kind = '') {
    hint.textContent = text;
    hint.className = kind ? `hint is-${kind}` : 'hint';
}

input.addEventListener('input', (e) => {
    let val = e.target.value.replace(/[^a-zA-Z0-9]/g, '').toUpperCase();
    if (val.length > 4) {
        val = val.slice(0, 4) + '-' + val.slice(4, 8);
    }
    e.target.value = val;
});

// 4. 实时状态：轮询配对窗口 + 倒计时
function renderHint() {
    if (paired) return;
    if (pairingExpiresAt === null) {
        setHint('配对未开放，请在服务端终端输入 pair', 'error');
        btn.disabled = true;
        return;
    }
    const remaining = pairingExpiresAt - Math.floor(Date.now() / 1000);
    if (remaining <= 0) {
        pairingExpiresAt = null;
        renderHint();
        return;
    }
    const m = String(Math.floor(remaining / 60)).padStart(2, '0');
    const s = String(remaining % 60).padStart(2, '0');
    setHint(`输入配对码以配对（${m}:${s} 后失效）`);
    if (!btn.classList.contains('is-loading')) {
        btn.disabled = false;
    }
}

async function loadState() {
    if (paired || stateLoadInFlight) return;
    stateLoadInFlight = true;
    try {
        const response = await fetch('/api/auth/state', {
            credentials: 'same-origin',
            cache: 'no-store',
        });
        const envelope = await response.json();
        if (envelope.data?.authenticated) {
            paired = true;
            window.location.reload();
            return;
        }
        pairingExpiresAt = envelope.data?.pairing_open
            ? envelope.data.pairing_expires_at ?? null
            : null;
        renderHint();
    } catch {
        setHint('无法连接服务器，正在重试…', 'error');
        btn.disabled = true;
    } finally {
        stateLoadInFlight = false;
        if (!paired) setTimeout(loadState, 5000);
    }
}

// 5. 提交处理
form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const code = input.value.replace(/[^a-zA-Z0-9]/g, '');
    if (code.length !== 8) {
        setHint('请输入完整的 8 位配对码', 'error');
        return;
    }

    // 按钮加载态
    btn.classList.add('is-loading');
    btn.disabled = true;
    input.disabled = true;

    try {
        const response = await fetch('/api/auth/pair', {
            method: 'POST',
            credentials: 'same-origin',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ code }),
        });
        const envelope = await response.json();
        if (!response.ok || envelope.code !== 0) {
            throw new Error(envelope.message || '配对失败');
        }
        paired = true;
        btn.classList.remove('is-loading');
        btn.classList.add('is-success');
        btnText.textContent = '配对成功 ✓';
        setHint('正在进入管理界面…', 'success');

        // 成功后的小动画（样式定义见 pair.css）
        card.classList.add('is-paired');

        setTimeout(() => window.location.reload(), 800);
    } catch (error) {
        setHint(error.message, 'error');
        btn.classList.remove('is-loading');
        btn.disabled = false;
        input.disabled = false;
        input.focus();
        loadState();
    }
});

loadState();
setInterval(renderHint, 1000);
