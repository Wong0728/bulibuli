<script setup lang="ts">
/**
 * Setup 向导页：首次启动或 onboarding 未完成时显示。
 *
 * 三步：
 *  1. 选择访问模式（local / lan / proxy）
 *  2. 选择网络访问策略（仅 lan 模式询问）
 *  3. 启用 AI Skill（可选）
 *
 * 提交后调用 /api/setup/apply，restart_required 时引导用户重启。
 */
import { ref, onMounted, defineEmits } from 'vue';
import { useSetupStore, type SetupMode } from '@/stores/setup';
import { useToastStore } from '@/stores/toast';
import type { SetupApplyResult } from '@/api/types';
import { setup as setupApi } from '@/api';

const setup = useSetupStore();
const toast = useToastStore();

const step = ref<1 | 2 | 3>(1);
const mode = ref<SetupMode>('local');
const accessDefault = ref<'allow' | 'deny'>('deny');
const proxyDomain = ref('');
const aiSkillEnabled = ref(true);
const restartRequired = ref(false);
const restartUrl = ref<string | null>(null);

onMounted(async () => {
  await setup.loadStatus();
  await setup.detectNetwork();
  if (setup.status) {
    mode.value = setup.status.mode;
    accessDefault.value = 'deny';
    proxyDomain.value = '';
    aiSkillEnabled.value = setup.status.ai_skill_enabled;
  }
});

function next() {
  if (step.value === 1) step.value = 2;
  else if (step.value === 2) step.value = 3;
}

async function finish() {
  // 先保存 AI Skill，再提交 onboarding 完成标记；apply 成功后只保留一次
  // finish 交接请求，避免 Setup 端口关闭时还有写请求未发出。
  const aiSaved = await setup.setAiSkill(aiSkillEnabled.value);
  if (!aiSaved) { toast.error(setup.error || 'AI Skill 设置保存失败，可稍后重试'); return; }
  const r: any = await setup.applyConfig({
    mode: mode.value,
    // 访问策略只对 LAN 模式有意义；local 模式回环始终放行（后端兜底），
    // proxy 模式来源由反代决定，均不传该字段。
    access_default: mode.value === 'lan' ? accessDefault.value : undefined,
    proxy_domain: proxyDomain.value || undefined,
    mark_completed: true,
  });
  if (!r) { toast.error(setup.error || '保存失败'); return; }
  if (r.restart_required) {
    restartRequired.value = true;
    restartUrl.value = r.main_url || null;
    toast.warn('配置已保存，需要重启应用后生效');
  } else {
    toast.success('配置已保存');
  }
  if (r.restart_required) return;

  const handoff = await waitForMainEndpoint(r);
  if (!handoff.main_url) {
    toast.error('主端口尚未就绪，请稍后重试；Setup 页面仍保持可用');
    return;
  }
  if (!await setup.finishHandoff()) {
    toast.error(setup.error || 'Setup 端口交接失败，请稍后重试');
    return;
  }
  emit('done', { ...r, main_url: handoff.main_url, accessible_urls: handoff.accessible_urls });
}

async function waitForMainEndpoint(initial: SetupApplyResult) {
  if (initial.main_url) return { main_url: initial.main_url, accessible_urls: initial.accessible_urls || [] };
  for (let attempt = 0; attempt < 20; attempt += 1) {
    await new Promise((resolve) => window.setTimeout(resolve, 250));
    try {
      const ports = await setupApi.ports();
      if (ports?.main_url) return ports;
    } catch { /* 主端口尚未绑定，继续等待 */ }
  }
  return { main_url: null, accessible_urls: [] };
}

const emit = defineEmits<{ (e: 'done', result: SetupApplyResult): void }>();
</script>

<template>
  <div class="setup-page">
    <div class="setup-container">
      <header class="setup-header">
        <h1><i class="fa-solid fa-rocket"></i> 欢迎使用补哩补哩</h1>
        <p>第一次启动？完成下面三步即可开始使用。</p>
      </header>

      <ol class="setup-steps">
        <li :class="{ active: step === 1, done: step > 1 }">1. 访问模式</li>
        <li :class="{ active: step === 2, done: step > 2 }">2. 访问策略</li>
        <li :class="{ active: step === 3 }">3. AI Skill</li>
      </ol>

      <!-- Step 1 -->
      <section v-if="step === 1" class="setup-step-body">
        <h2>选择访问模式</h2>
        <p class="form-note">决定本应用如何对外提供服务。</p>
        <div class="setup-mode-grid">
          <label :class="['setup-mode-card', { selected: mode === 'local' }]">
            <input v-model="mode" type="radio" value="local" />
            <i class="fa-solid fa-laptop"></i>
            <strong>仅本机</strong>
            <p>只监听 127.0.0.1，最安全。</p>
          </label>
          <label :class="['setup-mode-card', { selected: mode === 'lan' }]">
            <input v-model="mode" type="radio" value="lan" />
            <i class="fa-solid fa-network-wired"></i>
            <strong>局域网</strong>
            <p>监听所有网卡，局域网内可访问。</p>
          </label>
          <label :class="['setup-mode-card', { selected: mode === 'proxy' }]">
            <input v-model="mode" type="radio" value="proxy" />
            <i class="fa-solid fa-globe"></i>
            <strong>反向代理</strong>
            <p>部署在公网代理后面（需要域名）。</p>
          </label>
        </div>
        <div v-if="setup.detected" class="setup-detected">
          <span>检测到的 IP：</span>
          <code v-for="ip in setup.detected.ipv4" :key="ip">{{ ip }}</code>
          <code v-for="ip in setup.detected.ipv6" :key="ip">{{ ip }}</code>
        </div>
        <div class="setup-actions">
          <button class="btn btn-primary" @click="next">下一步</button>
        </div>
      </section>

      <!-- Step 2 -->
      <section v-else-if="step === 2" class="setup-step-body">
        <h2>访问策略</h2>
        <p class="form-note">{{ mode === 'lan' ? '局域网模式：默认仅允许白名单 IP 访问，也可选择放行全部内网设备。' : '本机/反代模式下本机访问始终放行，无需配置访问策略。' }}</p>
        <div v-if="mode === 'lan'" class="setup-radio-group">
          <label>
            <input v-model="accessDefault" type="radio" value="allow" />
            允许所有内网访问
          </label>
          <label>
            <input v-model="accessDefault" type="radio" value="deny" />
            仅允许白名单 IP
          </label>
        </div>
        <div v-if="mode === 'proxy'" class="form-group">
          <label for="setup-proxy-domain">代理域名</label>
          <input id="setup-proxy-domain" v-model="proxyDomain" class="form-control" placeholder="例如 bili.example.com" />
        </div>
        <div class="setup-actions">
          <button class="btn" @click="step = 1">上一步</button>
          <button class="btn btn-primary" @click="next">下一步</button>
        </div>
      </section>

      <!-- Step 3 -->
      <section v-else class="setup-step-body">
        <h2>AI Skill</h2>
        <p class="form-note">启用后可以在 <code>docs/skill.md</code> 复制 AI 协作说明。</p>
        <label class="choice-row">
          <span>启用 AI Skill</span>
          <span class="toggle-switch">
            <input v-model="aiSkillEnabled" type="checkbox" />
            <span class="slider"></span>
          </span>
        </label>
        <div v-if="setup.status?.ai_skill_path" class="form-note">
          路径：<code>{{ setup.status.ai_skill_path }}</code>
        </div>
        <div v-if="restartRequired" class="setup-restart-warning">
          <i class="fa-solid fa-triangle-exclamation"></i>
          <span>
            已保存，但访问模式变化需要重启应用。重启后请访问
            <a v-if="restartUrl" :href="restartUrl">{{ restartUrl }}</a>。
          </span>
        </div>
        <div class="setup-actions">
          <button class="btn" @click="step = 2">上一步</button>
          <button class="btn btn-primary" :disabled="setup.saving" @click="finish">
            {{ setup.saving ? '保存中…' : '完成' }}
          </button>
        </div>
      </section>
    </div>
  </div>
</template>

<style scoped>
.setup-page {
  min-height: 100vh;
  display: flex;
  align-items: center;
  justify-content: center;
  background: var(--color-bg, #f7f7f9);
  padding: 24px;
}
.setup-container {
  width: 100%;
  max-width: 720px;
  background: var(--color-surface, #fff);
  border-radius: 12px;
  padding: 32px;
  box-shadow: 0 4px 20px rgba(0,0,0,0.08);
}
.setup-header h1 {
  margin: 0 0 8px;
  font-size: 24px;
}
.setup-header p {
  color: var(--color-text-secondary, #666);
  margin: 0 0 24px;
}
.setup-steps {
  display: flex;
  gap: 16px;
  list-style: none;
  padding: 0;
  margin: 0 0 24px;
  border-bottom: 1px solid var(--color-border, #e5e5e5);
  padding-bottom: 12px;
}
.setup-steps li {
  color: var(--color-text-secondary, #999);
  font-size: 14px;
  padding: 4px 8px;
}
.setup-steps li.active {
  color: var(--color-primary, #00aeec);
  font-weight: 600;
}
.setup-steps li.done {
  color: var(--color-success, #28a745);
}
.setup-step-body h2 {
  margin: 0 0 8px;
  font-size: 18px;
}
.setup-mode-grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 12px;
  margin: 16px 0;
}
.setup-mode-card {
  display: block;
  padding: 16px;
  border: 2px solid var(--color-border, #e5e5e5);
  border-radius: 8px;
  cursor: pointer;
  text-align: center;
}
.setup-mode-card.selected {
  border-color: var(--color-primary, #00aeec);
  background: rgba(0, 174, 236, 0.05);
}
.setup-mode-card i {
  font-size: 24px;
  margin-bottom: 8px;
  display: block;
}
.setup-mode-card input {
  display: none;
}
.setup-detected {
  margin: 16px 0;
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: center;
  font-size: 13px;
  color: var(--color-text-secondary, #666);
}
.setup-detected code {
  background: var(--color-bg-secondary, #f0f0f0);
  padding: 2px 6px;
  border-radius: 4px;
}
.setup-radio-group {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin: 16px 0;
}
.setup-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  margin-top: 24px;
}
.setup-restart-warning {
  margin-top: 16px;
  padding: 12px;
  background: rgba(255, 193, 7, 0.1);
  border: 1px solid rgba(255, 193, 7, 0.3);
  border-radius: 8px;
  color: #b8860b;
  display: flex;
  align-items: center;
  gap: 8px;
}
</style>
