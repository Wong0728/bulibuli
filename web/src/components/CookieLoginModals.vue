<script setup lang="ts">
import { ref, onMounted, onUnmounted, watch } from 'vue';
import { useAuthStore } from '@/stores/auth';
import { useAppStore } from '@/stores/app';
import { useToastStore } from '@/stores/toast';
import { cookies as cookiesApi } from '@/api';
import type { QrcodeGenerate, QrcodePoll } from '@/api/types';

const auth = useAuthStore();
const app = useAppStore();
const toast = useToastStore();

const showQr = ref(false);
const qrcodeData = ref<QrcodeGenerate | null>(null);
const qrcodePollStatus = ref<QrcodePoll['status']>('pending');
const qrcodeMessage = ref('');
const qrcodeImage = ref<string | null>(null);
let pollTimer: number | null = null;

const showManualCookie = ref(false);
const manualCookieContent = ref('');

// 与全局 cookieLoginVisible 同步：App.vue 的"未登录"按钮 / TabSettings 的"扫码登录" 共享
watch(() => app.cookieLoginVisible, (v) => {
  if (v) {
    showQr.value = true;
    refreshQrcode();
  } else {
    showQr.value = false;
  }
});

function openQrLogin() {
  app.openCookieLogin();
}
function closeQrLogin() {
  showQr.value = false;
  app.closeCookieLogin();
  if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
}

async function refreshQrcode() {
  try {
    const data = await cookiesApi.qrcodeGenerate();
    if (!data) {
      toast.error('后端不可用，请检查服务是否运行');
      return;
    }
    qrcodeData.value = data;
    qrcodePollStatus.value = 'pending';
    qrcodeMessage.value = '请使用 B 站手机客户端扫码登录';
    qrcodeImage.value = (data as any).image_data || null;
    startPoll();
  } catch (e: any) {
    toast.error(e?.message || '生成二维码失败');
  }
}

function startPoll() {
  if (pollTimer) clearInterval(pollTimer);
  pollTimer = window.setInterval(async () => {
    if (!qrcodeData.value) return;
    try {
      const r: any = await cookiesApi.qrcodePoll(qrcodeData.value.qrcode_key);
      if (!r) return;
      // 后端 status 取值：pending | scanned | authenticated | expired | failed
      qrcodePollStatus.value = r.status;
      qrcodeMessage.value = r.message || qrcodeMessage.value;
      if (r.status === 'authenticated') {
        if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
        toast.success('扫码登录成功');
        await auth.refreshCookieStatus();
        closeQrLogin();
      } else if (r.status === 'expired' || r.status === 'failed') {
        if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
        if (r.status === 'expired') qrcodeMessage.value = '二维码已过期，请点击刷新';
      }
    } catch { /* 轮询忽略 */ }
  }, 2000);
}

function openManualCookie() {
  showManualCookie.value = true;
  manualCookieContent.value = '';
}
function closeManualCookie() {
  showManualCookie.value = false;
}

async function saveManualCookie() {
  if (!manualCookieContent.value.trim()) {
    toast.warn('请输入 Cookie 内容');
    return;
  }
  try {
    // 后端 /api/cookies/save 字段名是 `cookies`（不是 `content`），由 auth store 适配
    await auth.saveCookies(manualCookieContent.value);
    toast.success('Cookie 已保存');
    closeManualCookie();
  } catch (e: any) {
    toast.error(e?.message || '保存失败');
  }
}

onUnmounted(() => { if (pollTimer) clearInterval(pollTimer); });

defineExpose({ openQrLogin, openManualCookie, closeQrLogin, closeManualCookie });
</script>

<template>
  <!-- B 站扫码登录模态框（与原版 qrcode-modal 1:1 同构） -->
  <div v-if="showQr" id="qrcode-modal" class="modal-overlay" role="dialog" aria-modal="true" aria-labelledby="qrcode-modal-title" @click.self="closeQrLogin">
    <div class="modal-container qrcode">
      <div class="modal-header">
        <i class="fa-solid fa-bilibili text-brand"></i>
        <span id="qrcode-modal-title">B站扫码登录</span>
      </div>
      <div id="qrcode-container" class="qrcode-container">
        <div id="qrcode-canvas">
          <img v-if="qrcodeImage" :src="qrcodeImage" alt="QR Code" style="width: 220px; height: 220px;" />
        </div>
        <div id="qrcode-status" class="qrcode-status" :class="qrcodePollStatus">{{ qrcodeMessage }}</div>
      </div>
      <div id="qrcode-hint" class="qrcode-hint">请使用 Bilibili 手机客户端扫码</div>
      <div class="modal-footer">
        <button class="btn" data-action="close-qr-modal" @click="closeQrLogin">
          <i class="fa-solid fa-times"></i> 关闭
        </button>
        <button class="btn btn-primary" id="refresh-qrcode-btn" data-action="refresh-qr-code" @click="refreshQrcode">
          <i class="fa-solid fa-sync-alt"></i> 刷新二维码
        </button>
      </div>
    </div>
  </div>

  <!-- 手动粘贴 Cookie 模态框 -->
  <div v-if="showManualCookie" class="modal-overlay" role="dialog" aria-modal="true" @click.self="closeManualCookie">
    <div class="modal-container">
      <div class="modal-header">
        <i class="fa-solid fa-keyboard"></i>
        <span>手动输入 Cookie</span>
        <button type="button" class="modal-close-btn" aria-label="关闭" @click="closeManualCookie">
          <i class="fa-solid fa-times"></i>
        </button>
      </div>
      <div class="form-section">
        <div class="form-group form-full">
          <label>请从浏览器开发者工具中复制完整的 Cookie 字符串粘贴到下方：</label>
          <textarea v-model="manualCookieContent" class="form-control" rows="6" placeholder="DedeUserID=xxxx; SESSDATA=xxxx; ..."></textarea>
        </div>
      </div>
      <div class="modal-footer">
        <button class="btn" @click="closeManualCookie">
          <i class="fa-solid fa-times"></i> 取消
        </button>
        <button class="btn btn-primary" @click="saveManualCookie">
          <i class="fa-solid fa-check"></i> 保存
        </button>
      </div>
    </div>
  </div>
</template>
