/**
 * 认证 + Cookie 状态：
 * - 设备配对状态
 * - B 站登录状态（cookie 是否有效）
 * - 博主监控日志（来自 WS 推送）
 *
 * 所有 action 内部自带 try/catch，**不向调用者抛 promise reject**。
 */
import { defineStore } from 'pinia';
import { ref, computed } from 'vue';
import { auth as authApi, cookies as cookiesApi } from '@/api';
import type { AuthState, CookieStatus } from '@/api/types';

export interface BloggerLogEntry {
  ts: number;
  level: string;
  message: string;
}

export const useAuthStore = defineStore('auth', () => {
  const state = ref<AuthState>({ authenticated: false });
  const cookieStatus = ref<CookieStatus>({ configured: false, valid: false });
  const subscribedBloggerUid = ref<number | null>(null);
  const bloggerLogs = ref<BloggerLogEntry[]>([]);
  const knownBloggerChange = ref<Map<number, { name?: string; face?: string; ts: number }>>(new Map());
  const knownBloggers = ref<Array<{ uid: number; name: string; face?: string }>>([]);

  function setAuthState(s: AuthState) { state.value = s; }
  function setCookieStatus(s: CookieStatus) { cookieStatus.value = s; }

  async function refreshAuth() {
    try {
      const r = await authApi.state();
      if (r) state.value = r as AuthState;
      else state.value = { authenticated: false };
    } catch {
      state.value = { authenticated: false };
    }
  }

  async function refreshCookieStatus() {
    try {
      const r = await cookiesApi.status();
      if (r) cookieStatus.value = r as CookieStatus;
      else cookieStatus.value = { configured: false, valid: false };
    } catch {
      cookieStatus.value = { configured: false, valid: false };
    }
  }

  async function logout() {
    try { await authApi.logout(); } catch { /* 静默 */ }
    await refreshAuth();
  }

  async function saveCookies(content: string) {
    try {
      // 后端字段名是 `cookies`，不是 `content`
      await cookiesApi.save(content);
    } catch { /* 静默 */ }
    await refreshCookieStatus();
  }

  function appendBloggerLog(entry: BloggerLogEntry) {
    bloggerLogs.value.push(entry);
    if (bloggerLogs.value.length > 1000) bloggerLogs.value.splice(0, bloggerLogs.value.length - 1000);
  }

  function clearBloggerLogs() { bloggerLogs.value = []; }

  function setKnownBloggerChange(uid: number, payload: { name?: string; face?: string }) {
    knownBloggerChange.value.set(uid, { ...payload, ts: Date.now() });
    knownBloggerChange.value = new Map(knownBloggerChange.value);
  }

  function acknowledgeBloggerChange(uid: number) {
    knownBloggerChange.value.delete(uid);
    knownBloggerChange.value = new Map(knownBloggerChange.value);
  }

  function acknowledgeAllBloggerChanges() {
    knownBloggerChange.value.clear();
    knownBloggerChange.value = new Map();
  }

  function setKnownBloggers(list: Array<{ uid: number; name: string; face?: string }>) {
    knownBloggers.value = list;
  }

  const noticeCount = computed(() => knownBloggerChange.value.size);
  const isAuthenticated = computed(() => state.value.authenticated);
  const isCookieValid = computed(() => cookieStatus.value.configured && cookieStatus.value.valid);

  return {
    state, cookieStatus, subscribedBloggerUid, bloggerLogs, knownBloggerChange, knownBloggers,
    noticeCount, isAuthenticated, isCookieValid,
    setAuthState, setCookieStatus, refreshAuth, refreshCookieStatus, logout, saveCookies,
    appendBloggerLog, clearBloggerLogs, setKnownBloggerChange, acknowledgeBloggerChange, acknowledgeAllBloggerChanges,
    setKnownBloggers,
  };
});
