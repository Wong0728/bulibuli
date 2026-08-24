/**
 * Setup 向导状态：保存"配置是否完成"标志，驱动 App.vue 是否进入主界面。
 *
 * 后端约定（src/api/setup.rs）：
 *   - GET /api/setup/status → { completed, mode, configured_mode, restart_required,
 *     ai_skill_enabled, ai_skill_path, detected_ips, main_port, setup_port,
 *     main_url, accessible_urls }
 *   - POST /api/setup/apply → { mode, restart_required, main_port, ... }
 *   - POST /api/setup/ai-skill → { ai_skill_enabled }
 *   - GET /api/setup/detect → { ipv4: string[], ipv6: string[] }
 */
import { defineStore } from 'pinia';
import { ref } from 'vue';
import { setup as setupApi } from '@/api';
import type { SetupApplyResult } from '@/api/types';

export type SetupMode = 'local' | 'lan' | 'proxy';

export interface SetupStatus {
  completed: boolean;
  mode: SetupMode;
  configured_mode: SetupMode;
  restart_required: boolean;
  ai_skill_enabled: boolean;
  ai_skill_path: string;
  detected_ips: string[];
  main_port: number;
  setup_port: number;
  main_url: string | null;
  accessible_urls: string[];
}

export const useSetupStore = defineStore('setup', () => {
  const status = ref<SetupStatus | null>(null);
  const detected = ref<{ ipv4: string[]; ipv6: string[] } | null>(null);
  const saving = ref(false);
  const error = ref<string | null>(null);

  async function loadStatus(): Promise<SetupStatus | null> {
    try {
      const s: any = await setupApi.status();
      if (s) {
        status.value = s as SetupStatus;
        return status.value;
      }
    } catch { /* 静默 */ }
    return null;
  }

  async function detectNetwork() {
    try {
      const r: any = await setupApi.detect();
      detected.value = r;
    } catch { /* 静默 */ }
  }

  async function applyConfig(cfg: { mode: SetupMode; access_default?: 'allow' | 'deny'; proxy_domain?: string; mark_completed?: boolean }): Promise<SetupApplyResult | null> {
    saving.value = true;
    error.value = null;
    try {
      const r: any = await setupApi.apply(cfg);
      if (r) {
        // apply 的响应包含跨端口交接信息；不能在此重新请求 status，
        // 因为一次性 Setup 服务会在 finish 确认后关闭。
        return r as SetupApplyResult;
      }
      return null;
    } catch (e: any) {
      error.value = e?.message || '保存失败';
      return null;
    } finally {
      saving.value = false;
    }
  }

  async function finishHandoff() {
    try {
      return await setupApi.finish();
    } catch (e: any) {
      error.value = e?.message || 'Setup 端口交接失败';
      return null;
    }
  }

  async function setAiSkill(enabled: boolean) {
    try {
      const r: any = await setupApi.aiSkill(enabled);
      if (r && status.value) status.value.ai_skill_enabled = !!r.ai_skill_enabled;
      return true;
    } catch (e: any) {
      // 保存失败必须让用户知道，不能静默吞掉（否则用户以为 AI Skill 已启用）。
      error.value = e?.message || 'AI Skill 设置保存失败';
      return false;
    }
  }

  return {
    status, detected, saving, error,
    loadStatus, detectNetwork, applyConfig, finishHandoff, setAiSkill,
  };
});
