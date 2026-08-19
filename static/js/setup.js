// Setup 向导交互逻辑。

const API_BASE = window.location.origin;

// 状态
let selectedScene = null;
let selectedRemote = null;
let detectedIps = { ipv4: [], ipv6: [] };
let actualPorts = { main_port: 0, setup_port: 0, main_url: '', setup_url: '' };
let lastApplyResult = null;

// 初始化
document.addEventListener('DOMContentLoaded', () => {
    initStep1();
    initStep2();
    initStep3();
    void loadStatus();
});

async function loadStatus() {
    try {
        const [statusRes, portsRes] = await Promise.all([
            fetch(`${API_BASE}/api/setup/status`),
            fetch(`${API_BASE}/api/setup/ports`),
        ]);
        const json = await readSetupResponse(statusRes);
        actualPorts = (await readSetupResponse(portsRes)).data || actualPorts;
        if (json.code === 0) {
            // 如果已完成设置，显示提示
            if (json.data.completed) {
                showSetupNotice(
                    json.data.restart_required
                        ? '检测到网络模式已保存但尚未生效，请重启应用。'
                        : '检测到已有配置，重新保存后仍需按提示重启应用。',
                    json.data.restart_required ? 'warning' : 'info',
                );
            }
        }
    } catch (error) {
        showSetupNotice(`无法读取当前配置：${error.message || '服务暂不可用'}，请稍后重试。`, 'error');
    }
}

async function readSetupResponse(response) {
    let json;
    try {
        json = await response.json();
    } catch {
        throw new Error('服务返回了无法识别的响应');
    }
    if (!response.ok || json.code !== 0) {
        throw new Error(json.message || `请求失败（${response.status}）`);
    }
    return json;
}

function mainPortFallback() {
    const currentPort = Number.parseInt(window.location.port, 10);
    return Number.isFinite(currentPort) && currentPort > 1 ? currentPort - 1 : 5000;
}

function formatHost(ip) {
    const value = String(ip || '').replace(/^\[|\]$/g, '');
    return value.includes(':') ? `[${value}]` : value;
}

// --- 第 1 步：使用场景选择 ---
function initStep1() {
    const cards = document.querySelectorAll('.scene-card[data-scene]');
    const remoteCards = document.querySelectorAll('.scene-card[data-remote]');
    const remoteOptions = document.getElementById('remote-options');
    const nextBtn = document.getElementById('step1-next');

    const activateCard = card => {
            cards.forEach(c => c.classList.remove('selected'));
            cards.forEach(c => c.setAttribute('aria-pressed', 'false'));
            card.classList.add('selected');
            card.setAttribute('aria-pressed', 'true');
            selectedScene = card.dataset.scene;

            if (selectedScene === 'remote') {
                remoteOptions.hidden = false;
                nextBtn.disabled = true; // 需要选择远程方式
                selectedRemote = null;
            } else {
                remoteOptions.hidden = true;
                nextBtn.disabled = false;
                // 回退修正：切回本机/局域网时必须清掉残留的远程方式选择，
                // 否则第 2 步仍按 proxy 展示域名配置、甚至以 proxy 模式提交。
                selectedRemote = null;
                remoteCards.forEach(c => {
                    c.classList.remove('selected');
                    c.setAttribute('aria-pressed', 'false');
                });
            }
    };
    cards.forEach(card => {
        card.addEventListener('click', () => activateCard(card));
        card.addEventListener('keydown', event => {
            if (event.key === 'Enter' || event.key === ' ') {
                event.preventDefault();
                activateCard(card);
            }
        });
    });

    const activateRemoteCard = card => {
            remoteCards.forEach(c => c.classList.remove('selected'));
            remoteCards.forEach(c => c.setAttribute('aria-pressed', 'false'));
            card.classList.add('selected');
            card.setAttribute('aria-pressed', 'true');
            selectedRemote = card.dataset.remote;
            nextBtn.disabled = false;
    };
    remoteCards.forEach(card => {
        card.addEventListener('click', () => activateRemoteCard(card));
        card.addEventListener('keydown', event => {
            if (event.key === 'Enter' || event.key === ' ') {
                event.preventDefault();
                activateRemoteCard(card);
            }
        });
    });

    nextBtn.addEventListener('click', () => {
        goToStep(2);
        showConfigForScene();
    });
}

// --- 第 2 步：场景配置 ---
function initStep2() {
    document.getElementById('step2-back').addEventListener('click', () => goToStep(1));
    document.getElementById('step2-next').addEventListener('click', applySetup);

    // 域名输入 -> 显示 Caddy 配置
    const domainInput = document.getElementById('proxy-domain');
    domainInput.addEventListener('input', () => {
        const domain = domainInput.value.trim();
        const caddyConfig = document.getElementById('caddy-config');
        if (domain) {
            caddyConfig.hidden = false;
            document.getElementById('caddy-config-content').textContent =
`${domain} {
                reverse_proxy 127.0.0.1:${actualPorts.main_port || mainPortFallback()}
}`;
        } else {
            caddyConfig.hidden = true;
        }
    });

    // 复制 Caddy 配置
    document.getElementById('copy-caddy-btn').addEventListener('click', () => {
        const text = document.getElementById('caddy-config-content').textContent;
        copyToClipboard(text);
    });
}

function showConfigForScene() {
    // 隐藏所有配置区域
    document.querySelectorAll('.config-section').forEach(el => el.hidden = true);

    const scene = selectedRemote || selectedScene;
    const configEl = document.getElementById(`config-${scene}`);
    if (configEl) {
        configEl.hidden = false;
    }

    // 局域网模式：检测 IP
    if (scene === 'lan') {
        detectNetworkIps();
    } else if (scene === 'tunnel') {
        // 让用户清楚本应用监听哪个端口，便于穿透工具映射。
        const tunnelPort = actualPorts.main_port || mainPortFallback();
        document.querySelectorAll('.tunnel-port-marker, #tunnel-local-port').forEach(el => {
            el.textContent = String(tunnelPort);
        });
    }
}

async function detectNetworkIps() {
    const container = document.getElementById('detected-ips-lan');
    try {
        const [detectRes, portsRes] = await Promise.all([
            fetch(`${API_BASE}/api/setup/detect`),
            fetch(`${API_BASE}/api/setup/ports`),
        ]);
        const json = await readSetupResponse(detectRes);
        actualPorts = (await readSetupResponse(portsRes)).data || actualPorts;
        detectedIps = json.data;
        container.textContent = '';
        const title = document.createElement('p');
        title.innerHTML = '<strong>检测到的本机地址：</strong>';
        container.appendChild(title);
        const port = actualPorts.main_port || mainPortFallback();
        [...detectedIps.ipv4, ...detectedIps.ipv6].forEach(ip => {
            const div = document.createElement('div');
            div.className = 'ip-item';
            const code = document.createElement('code');
            code.textContent = `http://${formatHost(ip)}:${port}`;
            div.appendChild(code);
            container.appendChild(div);
        });
    } catch (error) {
        container.textContent = `网络检测失败：${error.message || '请稍后在设置页查看。'}`;
    }
}

async function applySetup() {
    const scene = selectedRemote || selectedScene;
    const body = {
        mark_completed: true,
    };

    switch (scene) {
        case 'local':
            body.mode = 'local';
            break;
        case 'lan':
            body.mode = 'lan';
            body.access_default = document.getElementById('lan-access-policy').value;
            break;
        case 'proxy':
            body.mode = 'proxy';
            body.proxy_domain = document.getElementById('proxy-domain').value.trim();
            if (!body.proxy_domain) {
                const input = document.getElementById('proxy-domain');
                input.setAttribute('aria-invalid', 'true');
                input.focus();
                alert('请输入域名');
                return;
            }
            document.getElementById('proxy-domain').removeAttribute('aria-invalid');
            break;
        case 'tunnel':
            body.mode = 'lan'; // 穿透工具映射到本地，服务端用 LAN 模式
            break;
    }

    try {
        const res = await fetch(`${API_BASE}/api/setup/apply`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(body),
        });
        const json = await readSetupResponse(res);
        lastApplyResult = json.data || {};
    } catch (error) {
        showSetupNotice(`配置请求失败：${error.message || '服务暂不可用'}`, 'error');
        return;
    }

    goToStep(3);
    showSummary(scene, lastApplyResult);
    if (lastApplyResult.restart_required) {
        showSetupNotice('网络模式已保存，重启应用后生效。当前页面会继续显示，完成后请重启程序。', 'warning');
    }
}

// --- 第 3 步：完成 ---
function initStep3() {
    const aiToggle = document.getElementById('ai-skill-toggle');
    const skillSection = document.getElementById('skill-path-section');

    aiToggle.addEventListener('change', async () => {
        const enabled = aiToggle.checked;
        skillSection.hidden = !enabled;

        // 保存 AI Skill 设置
        try {
            const response = await fetch(`${API_BASE}/api/setup/ai-skill`, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ enabled }),
            });
            const json = await readSetupResponse(response);
            if (json.data?.ai_skill_enabled !== enabled) {
                throw new Error('服务未确认 AI Skill 设置');
            }
            showSetupNotice(enabled ? 'AI Skill 已启用。' : 'AI Skill 已关闭。', 'success');
        } catch (error) {
            aiToggle.checked = !enabled;
            skillSection.hidden = !aiToggle.checked;
            showSetupNotice(`AI Skill 保存失败：${error.message || '请重试'}`, 'error');
        }
    });

    // 填充 AI Skill 绝对路径（后端返回；复制后可直接发给 AI）。
    const skillPathText = document.getElementById('skill-path-text');
    fetch(`${API_BASE}/api/setup/status`)
        .then(readSetupResponse)
        .then(json => {
            if (skillPathText && json.data?.ai_skill_path) {
                skillPathText.textContent = json.data.ai_skill_path;
            }
        })
        .catch(() => {});

    document.getElementById('copy-skill-path-btn').addEventListener('click', () => {
        const text = document.getElementById('skill-path-text').textContent;
        copyToClipboard(text);
    });

    document.getElementById('enter-main-btn').addEventListener('click', async () => {
        // 通过 API 获取实际主端口，避免端口 fallback 时计算错误
        try {
            const res = await fetch(`${API_BASE}/api/setup/ports`);
            const json = await res.json();
            if (json.code === 0 && json.data.main_url) {
                window.location.href = json.data.main_url;
                return;
            }
        } catch (error) {
            showSetupNotice(`无法获取主界面端口：${error.message || '将使用备用端口'}`, 'warning');
        }
        const mainPort = actualPorts.main_port || mainPortFallback();
        window.location.href = actualPorts.main_url || `http://127.0.0.1:${mainPort}`;
    });
}

function showSummary(scene, result = {}) {
    const modeTexts = {
        local: '仅本机',
        lan: '局域网',
        proxy: '反向代理',
        tunnel: '内网穿透',
    };
    const content = document.getElementById('summary-content');
    content.textContent = '';

    const row1Label = document.createElement('span');
    row1Label.className = 'summary-label';
    row1Label.textContent = '使用场景';
    const row1Value = document.createElement('span');
    row1Value.className = 'summary-value';
    row1Value.textContent = modeTexts[scene] || scene;
    const row1 = document.createElement('div');
    row1.className = 'summary-item';
    row1.appendChild(row1Label);
    row1.appendChild(row1Value);
    content.appendChild(row1);

    if (scene === 'proxy') {
        const domain = document.getElementById('proxy-domain').value.trim();
        const row2Label = document.createElement('span');
        row2Label.className = 'summary-label';
        row2Label.textContent = '域名';
        const row2Value = document.createElement('span');
        row2Value.className = 'summary-value';
        row2Value.textContent = domain;
        const row2 = document.createElement('div');
        row2.className = 'summary-item';
        row2.appendChild(row2Label);
        row2.appendChild(row2Value);
        content.appendChild(row2);
    }
    if (scene === 'lan') {
        const policy = document.getElementById('lan-access-policy').value;
        const row2Label = document.createElement('span');
        row2Label.className = 'summary-label';
        row2Label.textContent = '访问策略';
        const row2Value = document.createElement('span');
        row2Value.className = 'summary-value';
        row2Value.textContent = policy === 'allow' ? '全部允许' : '默认拒绝';
        const row2 = document.createElement('div');
        row2.className = 'summary-item';
        row2.appendChild(row2Label);
        row2.appendChild(row2Value);
        content.appendChild(row2);
    }
    if (result.restart_required) {
        const note = document.createElement('p');
        note.className = 'restart-required-note';
        note.textContent = '网络模式配置已保存，必须重启应用后才会切换监听方式。';
        content.appendChild(note);
    }
}

// --- 工具函数 ---
function goToStep(step) {
    // 更新步骤面板
    document.querySelectorAll('.step-panel').forEach(panel => panel.classList.remove('active'));
    document.getElementById(`step-${step}`).classList.add('active');

    // 更新步骤指示器
    document.querySelectorAll('.step-indicator .step').forEach(s => {
        const sStep = parseInt(s.dataset.step);
        s.classList.remove('active', 'completed');
        s.removeAttribute('aria-current');
        if (sStep === step) {
            s.classList.add('active');
            s.setAttribute('aria-current', 'step');
        } else if (sStep < step) {
            s.classList.add('completed');
        }
    });
    document.querySelector(`#step-${step} h2`)?.focus();
}

async function copyToClipboard(text) {
    try {
        if (navigator.clipboard?.writeText) {
            await navigator.clipboard.writeText(text);
        } else {
            const textarea = document.createElement('textarea');
            textarea.value = text;
            textarea.setAttribute('readonly', '');
            textarea.className = 'clipboard-fallback';
            document.body.appendChild(textarea);
            textarea.select();
            if (!document.execCommand('copy')) throw new Error('浏览器拒绝复制');
            textarea.remove();
        }
        showSetupNotice('已复制到剪贴板。', 'success');
    } catch (error) {
        showSetupNotice(`复制失败，请手动选择文本：${error.message || '浏览器未授权'}`, 'error');
    }
}

function showSetupNotice(message, tone = 'info') {
    const container = document.querySelector('.setup-container');
    if (!container) return;
    const notice = document.createElement('div');
    notice.className = `setup-notice setup-notice-${tone}`;
    notice.setAttribute('role', tone === 'error' ? 'alert' : 'status');
    notice.textContent = message;
    container.prepend(notice);
    window.setTimeout(() => notice.remove(), tone === 'error' ? 7000 : 4500);
}
