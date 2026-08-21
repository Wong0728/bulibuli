<script setup lang="ts">
import { ref, onUnmounted, watch, nextTick } from 'vue';
import { useAuthStore } from '@/stores/auth';
import { useAppStore } from '@/stores/app';
import { useToastStore } from '@/stores/toast';
import { cookies as cookiesApi } from '@/api';
import type { QrcodeGenerate } from '@/api/types';
import { useModalFocus } from '@/composables/modalFocus';

const auth = useAuthStore();
const app = useAppStore();
const toast = useToastStore();

const showQr = ref(false);
const qrcodeData = ref<QrcodeGenerate | null>(null);
const qrcodeCanvas = ref<HTMLCanvasElement | null>(null);
// 二维码生成阶段状态（对齐老框架 refreshQRCode 的容器 / hint / 刷新按钮控制）。
const qrLoading = ref(true);   // 获取中：canvas 容器显示 loading。
const qrFailed = ref(false);   // 生成失败：容器显示错误图标。
const qrHint = ref('正在获取二维码...');
const refreshBtnVisible = ref(false); // 老框架初始隐藏，仅失败/过期/异常时显示。
// 轮询状态框（对齐老框架 qrcode-status + setTone）。
const qrStatusVisible = ref(false);
const qrStatusText = ref('');
const qrStatusTone = ref<'brand' | 'success' | 'error' | ''>('');
let pollTimer: number | null = null;
let pollInFlight = false;
let qrGeneration = 0;

const qrRoot = ref<HTMLElement | null>(null);
useModalFocus(showQr, qrRoot, closeQrLogin);

// 与全局 cookieLoginVisible 同步：App.vue 的"未登录"按钮 / TabSettings 的"扫码登录" 共享
watch(() => app.cookieLoginVisible, (v) => {
  if (v) {
    showQr.value = true;
    refreshQrcode();
  } else {
    // 对齐老框架 closeQRCodeModal：关闭即作废轮询，防止弹窗关闭后仍在后台轮询。
    qrGeneration += 1;
    showQr.value = false;
    stopPoll();
  }
});

function openQrLogin() {
  app.openCookieLogin();
}
function closeQrLogin() {
  qrGeneration += 1;
  showQr.value = false;
  app.closeCookieLogin();
  stopPoll();
}

function stopPoll() {
  if (pollTimer) clearInterval(pollTimer);
  pollTimer = null;
  pollInFlight = false;
}

// 对齐老框架 bootstrap.js refreshQRCode。
async function refreshQrcode() {
  const generation = ++qrGeneration;
  stopPoll();
  qrcodeData.value = null;
  qrLoading.value = true;
  qrFailed.value = false;
  qrStatusVisible.value = false;
  qrHint.value = '正在获取二维码...';
  refreshBtnVisible.value = false;
  try {
    // 检查 QRCode 库是否加载（老框架在请求前检查）。
    const renderer = (window as any).QRCode;
    if (typeof renderer?.toCanvas !== 'function') {
      throw new Error('QRCode 库未加载，请检查网络连接或刷新页面');
    }
    const data = await cookiesApi.qrcodeGenerate();
    if (generation !== qrGeneration || !showQr.value) return;
    // 对齐老框架 getQRCodePayload：url / qrcode_key 必须是非空字符串。
    const url = typeof data?.url === 'string' ? data.url.trim() : '';
    const qrcodeKey = typeof data?.qrcode_key === 'string' ? data.qrcode_key.trim() : '';
    if (url && qrcodeKey) {
      qrLoading.value = false;
      await nextTick();
      // 老框架 toCanvas 参数：width 200 / margin 2 / 纯黑白。
      await renderer.toCanvas(qrcodeCanvas.value, url, {
        width: 200,
        margin: 2,
        color: { dark: '#000000', light: '#ffffff' },
      });
      if (generation !== qrGeneration) return;
      qrcodeData.value = { url, qrcode_key: qrcodeKey };
      qrHint.value = '请使用 Bilibili 手机客户端扫码';
      startPoll(generation);
    } else {
      // code=0 但缺字段的异常响应（不回显 'success'）。
      qrLoading.value = false;
      qrFailed.value = true;
      qrHint.value = '获取二维码失败：响应缺少必要字段，请重试';
      refreshBtnVisible.value = true;
    }
  } catch (e: any) {
    if (generation !== qrGeneration) return;
    console.error('[QRCode] 获取二维码失败:', e);
    qrLoading.value = false;
    qrFailed.value = true;
    qrHint.value = e?.message || '网络请求失败';
    refreshBtnVisible.value = true;
  }
}

// 对齐老框架 startPollingQRCode + qrcode-contract.js：
// 后端 status 已把 86090 归并进 pending，必须按 B 站原始 code 区分「已扫码」。
function startPoll(generation = qrGeneration) {
  if (generation !== qrGeneration) return;
  stopPoll();
  pollTimer = window.setInterval(async () => {
    if (generation !== qrGeneration || !qrcodeData.value || pollInFlight) return;
    pollInFlight = true;
    try {
      const r = await cookiesApi.qrcodePoll(qrcodeData.value.qrcode_key);
      if (generation !== qrGeneration) return;
      // 对齐 getQRCodePollState：code 非整数视为无效响应。
      const code = r != null && Number.isInteger(r.code) ? r.code : null;
      if (code === 0) {
        // 登录成功（后端已自动保存 cookie 到 DB，前端无需再持有）。
        stopPoll();
        qrStatusVisible.value = true;
        qrStatusTone.value = 'success';
        qrStatusText.value = '登录成功，账号信息已更新。';
        toast.success('扫码登录成功');
        // 账号信息刷新完成后短暂保留成功反馈，再关闭弹窗（老框架停留约 1.5s）。
        void auth.refreshCookieStatus();
        setTimeout(() => {
          if (generation === qrGeneration) closeQrLogin();
        }, 1500);
      } else if (code === 86101) {
        // 未扫码：状态框隐藏。
        qrStatusVisible.value = false;
      } else if (code === 86090) {
        // 已扫码未确认。
        qrStatusText.value = r?.message || '已扫码，请在手机上确认';
        qrStatusTone.value = 'brand';
        qrStatusVisible.value = true;
      } else if (code === 86038) {
        // 二维码失效。
        stopPoll();
        qrStatusText.value = r?.message || '二维码已失效';
        qrStatusTone.value = 'error';
        qrStatusVisible.value = true;
        refreshBtnVisible.value = true;
      } else {
        // 状态异常（含 code 缺失：轮询响应缺少状态码）。
        stopPoll();
        qrStatusText.value = code == null
          ? '二维码轮询响应缺少状态码'
          : (r?.message || `二维码状态异常（代码 ${code}），请刷新重试`);
        qrStatusTone.value = 'error';
        qrStatusVisible.value = true;
        refreshBtnVisible.value = true;
      }
    } catch (e: any) {
      if (generation !== qrGeneration) return;
      console.error('轮询状态失败:', e);
      stopPoll();
      qrStatusText.value = `轮询二维码状态失败：${e?.message || '请刷新重试'}`;
      qrStatusTone.value = 'error';
      qrStatusVisible.value = true;
      refreshBtnVisible.value = true;
    } finally {
      pollInFlight = false;
    }
  }, 2000);
}

onUnmounted(() => { qrGeneration += 1; stopPoll(); });

// 手动粘贴 Cookie 入口对齐老框架（auth-card.js / settings.html）：它是设置页
// account section 内的折叠面板（TabSettings.vue 内联实现），不是独立模态。
// 此组件只保留扫码登录模态，避免双入口。
defineExpose({ openQrLogin, closeQrLogin });
</script>

<template>
  <!-- B 站扫码登录模态框（DOM 对齐老框架 index.html qrcode-modal） -->
  <div v-if="showQr" ref="qrRoot" id="qrcode-modal" class="modal-overlay" role="dialog" aria-modal="true" aria-labelledby="qrcode-modal-title" @click.self="closeQrLogin">
    <div class="modal-container qrcode">
      <div class="modal-header">
        <i class="fa-solid fa-bilibili text-brand"></i>
        <span id="qrcode-modal-title">B站扫码登录</span>
      </div>
      <div id="qrcode-container" class="qrcode-container">
        <div id="qrcode-canvas">
          <div v-if="qrLoading" class="loading"></div>
          <i v-else-if="qrFailed" class="fa-solid fa-exclamation-circle fa-3x status-error"></i>
          <canvas v-else ref="qrcodeCanvas" aria-label="B站登录二维码"></canvas>
        </div>
        <div v-if="qrStatusVisible" id="qrcode-status" class="qrcode-status" :class="qrStatusTone ? `tone-${qrStatusTone}` : ''">{{ qrStatusText }}</div>
      </div>
      <div id="qrcode-hint" class="qrcode-hint">{{ qrHint }}</div>
      <div class="modal-footer">
        <button class="btn" data-action="close-qr-modal" @click="closeQrLogin">
          <i class="fa-solid fa-times"></i> 关闭
        </button>
        <button v-if="refreshBtnVisible" class="btn btn-primary" id="refresh-qrcode-btn" data-action="refresh-qr-code" @click="refreshQrcode">
          <i class="fa-solid fa-sync-alt"></i> 刷新二维码
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
/* 老框架 .tone-brand（已扫码的品牌色反馈）与 .tone-error（错误红字）。
   新框架全局样式只有 qrcode-status 的 tone-success / expired / partial，这里补齐。 */
.qrcode-status.tone-brand {
  color: var(--surface);
  background: var(--brand);
}

.qrcode-status.tone-error {
  color: var(--error);
  background: var(--error-soft);
  border-color: color-mix(in srgb, var(--error) 35%, transparent);
}
</style>
