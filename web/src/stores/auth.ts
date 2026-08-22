/**
 * 认证 + Cookie 状态：
 * - 设备配对状态
 * - B 站登录状态（cookie 是否有效）
 * - 博主改名/换头像通知
 *
 * 所有 action 内部自带 try/catch，**不向调用者抛 promise reject**。
 */
import { defineStore } from 'pinia';
import { ref, computed } from 'vue';
import { auth as authApi, cookies as cookiesApi } from '@/api';
import { setCsrfToken, postFull, get, NETWORK_ERR_MSG } from '../api/client';
import { useToastStore } from './toast';
import type { AuthState, CookieStatus } from '@/api/types';

export const useAuthStore = defineStore('auth', () => {
  const toast = useToastStore();
  const state = ref<AuthState>({ authenticated: false });
  const cookieStatus = ref<CookieStatus>({ configured: false, has_cookies: false, valid: false });
  const cookieStatusLoaded = ref(false);
  const knownBloggerChange = ref<Map<number, { name?: string; face?: string; ts: number }>>(new Map());

  function setAuthState(s: AuthState) {
    state.value = s;
    // csrf token 不再经公开的 /api/auth/state 下发；认证后由 refreshCsrfToken 获取。
    setCsrfToken(null);
  }

  /** 从认证端点 /api/auth/csrf 获取当前会话的 CSRF token（写请求共用）。 */
  async function refreshCsrfToken() {
    try {
      const r = await get<{ csrf_token?: string }>('/api/auth/csrf');
      setCsrfToken(r?.csrf_token ?? null);
    } catch {
      setCsrfToken(null);
    }
  }
  function normalizeCookieStatus(raw?: Partial<CookieStatus> | null): CookieStatus {
    const hasCookies = raw?.has_cookies ?? raw?.configured ?? false;
    return {
      ...raw,
      configured: hasCookies,
      has_cookies: hasCookies,
      valid: raw?.valid === true,
    };
  }

  function setCookieStatus(s: CookieStatus | null) {
    const normalized = normalizeCookieStatus(s);
    cookieStatus.value = normalized;
    cookieStatusLoaded.value = true;
    if (state.value.authenticated && normalized.valid && normalized.uname) {
      state.value = {
        ...state.value,
        user: {
          mid: normalized.mid,
          name: normalized.uname,
          face: normalized.face,
        },
      };
    }
  }

  async function refreshAuth() {
    try {
      const r = await authApi.state();
      if (r) setAuthState(r as AuthState);
      else setAuthState({ authenticated: false });
      if ((r as AuthState | null)?.authenticated) await refreshCsrfToken();
    } catch {
      setAuthState({ authenticated: false });
    }
  }

  async function pair(code: string, deviceName = '') {
    const result = await authApi.pair(code, deviceName || undefined);
    if (!(result as any)?.paired) throw new Error('配对未完成，请检查配对码');
    // 配对响应已经写入 session cookie；由 App 重新拉取 auth/state，
    // 一次性取得真实的 CSRF token 和会话角色，避免中间态触发入口抖动。
    return true;
  }

  async function refreshCookieStatus() {
    try {
      const r = await cookiesApi.status();
      setCookieStatus(r as CookieStatus | null);
    } catch {
      // 状态检查是上游请求；失败时保留“已配置”事实，避免把临时网络问题闪成未登录。
      setCookieStatus({ ...cookieStatus.value, valid: false, state: 'unreachable' });
    }
  }

  /** 终态挂起：成功后页面即将整页 reload，失败时也不允许调用方继续执行 await 后的语句
   * （防止 TabSettings 过渡态弹过时的成功 toast）。用户重新点击按钮会发起新调用，无死锁。 */
  const NEVER = new Promise<never>(() => {});

  async function logoutAccount() {
    // 对齐老框架 bootstrap.js logoutAccount：单步「清 B 站 Cookie → 注销设备会话 → 整页 reload」。
    // 确认弹窗由调用方负责（老框架文案：确定要退出当前 B 站账号登录吗？退出后需重新扫码或粘贴 Cookie。）
    try {
      // 先清 B 站 Cookie（此时会话仍有效）。client 在 code!==0 时抛错，
      // 等价老框架 cookieResult.code !== 0 → toast(message || '退出失败') 分支。
      await postFull('/api/cookies/save', { cookies: '' });
      // 注销当前设备会话（撤销配对令牌 + 断开 WS）。只清 Cookie 会让会话在
      // 有效期内仍可用，必须补调后端 /api/auth/logout。
      await postFull('/api/auth/logout', {});
      toast.success('已退出登录，设备会话已注销');
      // 会话 Cookie 已被后端清除，刷新回到配对/登录页。
      window.location.reload();
    } catch (e: any) {
      // 对齐老框架 formatError('退出登录', e)：网络失败用全局网络文案，其余用后端 message。
      toast.error(e?.code === 0 ? NETWORK_ERR_MSG : `退出登录：${e?.message || '未知错误'}`);
    }
    await NEVER;
  }

  /** 旧入口别名：TabSettings（白名单外）仍调用这两个名字，行为统一为完整退出登录。 */
  async function logoutDevice() { await logoutAccount(); }
  async function logoutBiliAccount() { await logoutAccount(); }

  async function saveCookies(content: string) {
    // 后端字段名是 `cookies`，不是 `content`；主动保存失败必须交给 UI 提示。
    await cookiesApi.save(content);
    await refreshCookieStatus();
  }

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

  const noticeCount = computed(() => knownBloggerChange.value.size);
  const isAuthenticated = computed(() => state.value.authenticated);
  const isCookieValid = computed(() => cookieStatus.value.has_cookies === true && cookieStatus.value.valid === true);
  const biliUser = computed(() => cookieStatus.value.valid ? {
    mid: cookieStatus.value.mid,
    name: cookieStatus.value.uname,
    face: cookieStatus.value.face,
  } : null);

  return {
    state, cookieStatus, cookieStatusLoaded, biliUser, knownBloggerChange,
    noticeCount, isAuthenticated, isCookieValid,
    setAuthState, setCookieStatus, refreshAuth, refreshCsrfToken, pair, refreshCookieStatus, logoutAccount, logoutDevice, logoutBiliAccount, saveCookies,
    setKnownBloggerChange, acknowledgeBloggerChange, acknowledgeAllBloggerChanges,
  };
});
