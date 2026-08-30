<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue';
import { auth as authApi } from '@/api';
import { useAuthStore } from '@/stores/auth';

const emit = defineEmits<{ paired: [] }>();
const auth = useAuthStore();
const code = ref('');
const pairingOpen = ref(false);
const expiresAt = ref<number | null>(null);
const loading = ref(false);
const polling = ref(false);
const error = ref('');
/** 最近一次轮询产生的错误文案：只有它自己能在轮询恢复后被清掉。 */
const pollError = ref<string | null>(null);
const paired = ref(false);
const now = ref(Math.floor(Date.now() / 1000));
/** 服务器时钟偏移（server_time - 本地时间）：倒计时用本地时钟 + 偏移校准。 */
const clockOffset = ref(0);
let timer: number | null = null;
let clock: number | null = null;

const floatingBits = [
  'BVID', 'UID', 'API:OK', 'GET /video', 'WS:SYNC', 'CLOUD',
  '1080P', 'BV1xx...', 'RE-UP', 'TASK:START', 'DB:SAVE',
  'aria2', 'ffmpeg', 'DANMAKU', 'MONITOR', 'COOKIE:OK',
  'SESSION', 'ARCHIVE', 'AV->BV', 'HEVC', 'SUB:BURN', 'RETRY:3',
];
const heroSlots = Array.from({ length: 28 }, (_, index) => index);

const serverNow = computed(() => now.value + clockOffset.value);
const remaining = computed(() => expiresAt.value == null ? 0 : Math.max(0, expiresAt.value - serverNow.value));
const hint = computed(() => {
  if (paired.value) return '正在进入管理界面…';
  if (error.value) return error.value;
  if (!pairingOpen.value || remaining.value <= 0)
    return '配对未开放：在服务端终端输入 pair 打开配对；找不到配对码时执行 bulibuli ctl pair 可查看/重新生成';
  const m = String(Math.floor(remaining.value / 60)).padStart(2, '0');
  const s = String(remaining.value % 60).padStart(2, '0');
  if (remaining.value <= 60) return `配对码即将过期（剩 ${m}:${s}），请尽快输入`;
  return `输入配对码以配对（${m}:${s} 后失效）`;
});
const hintClass = computed(() => {
  if (paired.value) return 'is-success';
  if (error.value || !pairingOpen.value || remaining.value <= 0) return 'is-error';
  return remaining.value <= 60 ? 'is-warn' : '';
});
const buttonText = computed(() => paired.value ? '配对成功 ✓' : '确认配对');

/** 与服务端 `src/services/auth.rs::CODE_ALPHABET` 一致的配对码字母表（已排除易混的 0/O/1/I/L）。 */
const CODE_ALPHABET = '23456789ABCDEFGHJKMNPQRSTUVWXYZ';
/** 整段粘贴（终端横幅、`ctl pair` 的 JSON 输出）时精确抠出配对码，避免把相邻文本当码。 */
const CODE_PATTERN = new RegExp(`[${CODE_ALPHABET}]{4}-?[${CODE_ALPHABET}]{4}`);

function formatCode(value: string) {
  const extracted = value.match(CODE_PATTERN)?.[0] ?? value;
  const normalized = extracted.replace(/[^a-z0-9]/gi, '').toUpperCase().slice(0, 8);
  return normalized.length > 4 ? `${normalized.slice(0, 4)}-${normalized.slice(4)}` : normalized;
}

function onCodeInput(event: Event) {
  code.value = formatCode((event.target as HTMLInputElement).value);
}

/** 粘贴要单独处理：`maxlength=9` 会让浏览器把整段文本截断在前 9 个字符，
 *  @input 只能看到残缺片段（如 `{"data":{`）。这里直接读剪贴板全文取码。 */
function onCodePaste(event: ClipboardEvent) {
  const pasted = event.clipboardData?.getData('text') ?? '';
  if (!pasted || !CODE_PATTERN.test(pasted)) return;
  event.preventDefault();
  code.value = formatCode(pasted);
}

function finishPairing() {
  if (paired.value) return;
  paired.value = true;
  loading.value = false;
  emit('paired');
}

async function confirmSession() {
  // 配对响应写入 HttpOnly Cookie 后，立即读取状态可能落在浏览器/服务端
  // 的边界时序；保持当前配对页不动，短暂重试直到拿到真实会话。
  for (let attempt = 0; attempt < 6; attempt += 1) {
    try {
      const state = await authApi.state();
      if (state?.authenticated) {
        auth.setAuthState(state);
        return true;
      }
    } catch { /* 继续短暂重试 */ }
    if (attempt < 5) {
      await new Promise<void>(resolve => window.setTimeout(resolve, Math.min(1000, 150 * (2 ** attempt))));
    }
  }
  return false;
}

async function loadState() {
  if (paired.value || polling.value || loading.value) return;
  polling.value = true;
  try {
    const state = await authApi.state();
    if (!state) throw new Error('无法连接服务器，正在重试…');
    if (state?.authenticated) {
      auth.setAuthState(state);
      finishPairing();
      return;
    }
    pairingOpen.value = !!state?.pairing_open;
    expiresAt.value = state?.pairing_expires_at ?? null;
    // 用服务器时间校准时钟偏移：客户端时钟偏差不再影响倒计时准确性。
    if (typeof state?.server_time === 'number') {
      clockOffset.value = state.server_time - Math.floor(Date.now() / 1000);
    }
    // 只清理轮询自己产生的错误：提交失败的原因要保留到下一次提交，
    // 否则后端刚返回的「配对码无效」会在同一轮里被抹掉。
    if (pollError.value !== null && error.value === pollError.value) error.value = '';
    pollError.value = null;
    restoreFastPolling();
  } catch (e: any) {
    const message: string = e?.message || '无法连接服务器，正在重试…';
    pollError.value = message;
    error.value = message;
    slowDownPolling();
  } finally {
    polling.value = false;
  }
}

/** 轮询退避：请求失败（如触发限流 429）时放慢到 15s，避免失败请求持续
 * 打满服务端探测限流窗口；恢复成功后回到 5s 常规节奏。 */
const FAST_POLL_MS = 5000;
const SLOW_POLL_MS = 15000;
let pollIntervalMs = FAST_POLL_MS;

function restartPollTimer() {
  if (timer) clearInterval(timer);
  timer = window.setInterval(() => void loadState(), pollIntervalMs);
}

function slowDownPolling() {
  if (pollIntervalMs === SLOW_POLL_MS) return;
  pollIntervalMs = SLOW_POLL_MS;
  restartPollTimer();
}

function restoreFastPolling() {
  if (pollIntervalMs === FAST_POLL_MS) return;
  pollIntervalMs = FAST_POLL_MS;
  restartPollTimer();
}

async function submit() {
  const normalized = code.value.replace(/[^a-z0-9]/gi, '');
  if (normalized.length !== 8) {
    error.value = '请输入完整的 8 位配对码';
    return;
  }
  if (!pairingOpen.value || remaining.value <= 0) {
    error.value = '当前未开放设备配对：在服务端终端输入 pair，或执行 bulibuli ctl pair 重新生成配对码';
    return;
  }
  loading.value = true;
  error.value = '';
  try {
    const authenticated = await auth.pair(normalized, 'Vue3 Web');
    if (!authenticated || !(await confirmSession())) throw new Error('配对成功但会话尚未生效，请稍后重试');
    finishPairing();
  } catch (e: any) {
    loading.value = false;
    error.value = e?.message || '配对失败，请检查配对码';
    await loadState();
  }
}

onMounted(() => {
  void loadState();
  restartPollTimer();
  clock = window.setInterval(() => { now.value = Math.floor(Date.now() / 1000); }, 1000);
});
onUnmounted(() => {
  if (timer) clearInterval(timer);
  if (clock) clearInterval(clock);
});
</script>

<template>
  <main class="pair-page">
    <div class="stage" aria-hidden="true">
      <span v-for="index in heroSlots" :key="index" class="floating-bit">{{ floatingBits[index % floatingBits.length] }}</span>
    </div>

    <section class="pairing-card" :class="{ 'is-paired': paired }" aria-labelledby="pair-title">
      <div class="header-graphic">
        <svg class="connect-svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
          <path d="M5 12h14M12 5l7 7-7 7" stroke-dasharray="4 2" />
          <circle class="dot-pulse" cx="19" cy="12" r="2" fill="currentColor" />
          <circle cx="5" cy="12" r="2" fill="currentColor" />
        </svg>
      </div>

      <h1 id="pair-title">补哩补哩远程配对</h1>
      <p class="hint" :class="hintClass" aria-live="polite">{{ hint }}</p>

      <form @submit.prevent="submit">
        <div class="input-group">
          <label class="sr-only" for="pair-code">配对码</label>
           <input id="pair-code" :value="code" @input="onCodeInput" @paste="onCodePaste"
                 class="pairing-input" placeholder="XXXX-XXXX" maxlength="9" autocomplete="one-time-code"
                 spellcheck="false" :disabled="loading || paired" />
        </div>

        <button type="submit" class="btn-submit" :class="{ 'is-loading': loading, 'is-success': paired }"
                :disabled="loading || paired || !pairingOpen || remaining <= 0">
          <div class="loading-spinner" aria-hidden="true"></div>
          <span class="btn-text">{{ buttonText }}</span>
        </button>
      </form>
    </section>
  </main>
</template>

<style scoped>
.pair-page {
  position: relative;
  display: flex;
  align-items: center;
  justify-content: center;
  min-height: 100vh;
  min-height: 100dvh;
  overflow: hidden;
  padding: 1rem;
  background: var(--bg);
  color: var(--text);
}

/* Hero 背景保留旧版的数据碎片；入口本身保持静态，避免用户看到持续闪烁。 */
.stage {
  position: absolute;
  inset: 0;
  z-index: 1;
  pointer-events: none;
}

.floating-bit {
  position: absolute;
  padding: 4px 8px;
  border: 1px solid var(--brand);
  border-radius: 4px;
  color: var(--brand);
  font-family: var(--font-mono);
  font-size: 11px;
  font-weight: 500;
  letter-spacing: 0.02em;
  opacity: 0.15;
  user-select: none;
  white-space: nowrap;
  will-change: transform;
}

.floating-bit:nth-child(1) { left: 4%; top: 8%; }
.floating-bit:nth-child(2) { left: 18%; top: 15%; }
.floating-bit:nth-child(3) { left: 32%; top: 6%; }
.floating-bit:nth-child(4) { left: 55%; top: 4%; }
.floating-bit:nth-child(5) { left: 72%; top: 9%; }
.floating-bit:nth-child(6) { left: 88%; top: 14%; }
.floating-bit:nth-child(7) { left: 92%; top: 30%; }
.floating-bit:nth-child(8) { left: 6%; top: 28%; }
.floating-bit:nth-child(9) { left: 13%; top: 42%; }
.floating-bit:nth-child(10) { left: 88%; top: 45%; }
.floating-bit:nth-child(11) { left: 3%; top: 55%; }
.floating-bit:nth-child(12) { left: 12%; top: 68%; }
.floating-bit:nth-child(13) { left: 87%; top: 62%; }
.floating-bit:nth-child(14) { left: 92%; top: 76%; }
.floating-bit:nth-child(15) { left: 8%; top: 82%; }
.floating-bit:nth-child(16) { left: 20%; top: 90%; }
.floating-bit:nth-child(17) { left: 35%; top: 94%; }
.floating-bit:nth-child(18) { left: 52%; top: 91%; }
.floating-bit:nth-child(19) { left: 68%; top: 95%; }
.floating-bit:nth-child(20) { left: 82%; top: 88%; }
.floating-bit:nth-child(21) { left: 25%; top: 3%; }
.floating-bit:nth-child(22) { left: 45%; top: 10%; }
.floating-bit:nth-child(23) { left: 64%; top: 88%; }
.floating-bit:nth-child(24) { left: 79%; top: 24%; }
.floating-bit:nth-child(25) { left: 2%; top: 16%; }
.floating-bit:nth-child(26) { left: 90%; top: 5%; }
.floating-bit:nth-child(27) { left: 5%; top: 93%; }
.floating-bit:nth-child(28) { left: 93%; top: 91%; }

.pairing-card {
  position: relative;
  z-index: 10;
  width: 90%;
  max-width: 400px;
  padding: 40px;
  border: 1px solid var(--border);
  border-radius: 24px;
  background: var(--surface);
  box-shadow: 0 12px 32px rgba(0, 0, 0, 0.05);
  text-align: center;
}

.pairing-card.is-paired {
  transform: scale(1.05);
  transition: transform 0.5s cubic-bezier(0.175, 0.885, 0.32, 1.275);
}

.header-graphic { margin-bottom: 24px; }
.connect-svg { width: 64px; height: 64px; color: var(--brand); }
.dot-pulse { opacity: 0.65; animation: dot-pulse 1.6s ease-in-out infinite; }
@keyframes dot-pulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.3; }
}

h1 {
  margin: 0 0 8px;
  color: var(--text);
  font-size: 20px;
  font-weight: 600;
}

.hint {
  min-height: 21px;
  margin: 0 0 32px;
  color: var(--text-secondary);
  font-size: 14px;
  transition: color 0.25s;
}
.hint.is-error { color: var(--error); }
.hint.is-warn { color: var(--warning); font-weight: 600; }
.hint.is-success { color: var(--success); }

.input-group { margin-bottom: 20px; }
.pairing-input {
  width: 100%;
  height: 60px;
  border: 2px solid var(--border);
  border-radius: 12px;
  background: var(--bg);
  color: var(--text);
  font-family: var(--font-mono);
  font-size: 24px;
  font-weight: 500;
  letter-spacing: 4px;
  outline: none;
  text-align: center;
  text-transform: uppercase;
  transition: all 0.25s;
}
.pairing-input:focus {
  border-color: var(--brand);
  background: var(--surface);
  box-shadow: 0 0 0 4px var(--brand-soft);
}
.pairing-input::placeholder {
  color: var(--text-muted);
  font-family: var(--font-mono);
  font-size: 24px;
  font-weight: 500;
  letter-spacing: 4px;
}
.pairing-input:disabled { cursor: not-allowed; opacity: 0.6; }

.btn-submit {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 100%;
  height: 52px;
  gap: 8px;
  border: 0;
  border-radius: 12px;
  background: var(--brand);
  color: #fff;
  cursor: pointer;
  font-size: 16px;
  font-weight: 600;
  transition: all 0.2s;
}
.btn-submit:hover:not(:disabled) {
  background: var(--brand-hover);
  box-shadow: 0 4px 12px rgba(251, 114, 153, 0.3);
  transform: translateY(-1px);
}
.btn-submit:active { transform: translateY(0); }
.btn-submit:disabled { cursor: not-allowed; opacity: 0.6; }
.btn-submit.is-success { background: var(--success); opacity: 1; }

.loading-spinner {
  display: none;
  width: 20px;
  height: 20px;
  border: 2px solid rgba(255, 255, 255, 0.3);
  border-top-color: #fff;
  border-radius: 50%;
  animation: pair-spin 0.8s linear infinite;
}
@keyframes pair-spin { to { transform: rotate(360deg); } }
.is-loading .loading-spinner { display: block; }
.is-loading .btn-text { display: none; }

@media (max-width: 480px) {
  .pairing-card { padding: 32px 24px; }
}

@media (prefers-reduced-motion: reduce) {
  .pairing-card,
  .pairing-card.is-paired,
  .dot-pulse,
  .loading-spinner { animation-duration: 0.01ms; animation-iteration-count: 1; transition-duration: 0.01ms; }
}
</style>
