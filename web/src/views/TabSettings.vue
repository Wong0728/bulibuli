<script setup lang="ts">
/**
 * 设置页：保留迁移前已验证的交互语义。
 * 本组件承载页面骨架（分组切换 / 折叠委托 / 保存与重置 / 轮询生命周期）；
 * 各分组 sections 拆在 SettingsBasicSections / SettingsAria2Sections /
 * SettingsDownloadSections / SettingsAdvancedSections / SettingsToolSections。
 */
import { ref, computed, watch, onActivated, onDeactivated, onUnmounted, nextTick } from 'vue';
import { useSettingsStore } from '@/stores/settings';
import { useAuthStore } from '@/stores/auth';
import { useToastStore } from '@/stores/toast';
import { useDownloadStore } from '@/stores/download';
import { confirmDialog } from '@/composables/confirm';
import { useAppStore } from '@/stores/app';
import { autoScrollToBottomIfPinned } from '@/utils/scroll';
import { useSettingsSections, GROUP_BUTTONS } from '@/composables/useSettingsSections';
import { useGlobalLogs } from '@/composables/useGlobalLogs';
import { useFoundationSummary } from '@/composables/useFoundationSummary';
import { useSettingsUpdate } from '@/composables/useSettingsUpdate';
import SettingsBasicSections from '@/components/SettingsBasicSections.vue';
import SettingsAria2Sections from '@/components/SettingsAria2Sections.vue';
import SettingsDownloadSections from '@/components/SettingsDownloadSections.vue';
import SettingsAdvancedSections from '@/components/SettingsAdvancedSections.vue';
import SettingsToolSections from '@/components/SettingsToolSections.vue';

const settings = useSettingsStore();
const auth = useAuthStore();
const toast = useToastStore();
const download = useDownloadStore();

// 扫码登录：通过 app store 共享开关（顶部"未登录"按钮也共用）
const app = useAppStore();
const isOwner = computed(() => auth.state.role === 'owner');

const { group, setGroup, onSectionsClick, onSectionsKeydown } = useSettingsSections();
const { startGlobalLogsPolling, stopGlobalLogsPolling } = useGlobalLogs();
const { loadFoundationSummary } = useFoundationSummary();
const { updateStatusLoaded } = useSettingsUpdate();

// aria2 健康四态点：页面停留期间每 10s 跟随共享 health 缓存刷新，避免陈旧状态。
let healthTimer: number | null = null;
function startHealthPolling() {
  if (healthTimer) return;
  healthTimer = window.setInterval(() => {
    if (document.hidden) return;
    if (app.currentTab !== 'settings') return;
    void download.refreshHealth();
  }, 10000);
}

// 本页挂在 App.vue 的 KeepAlive 下：onUnmounted 在切 Tab 时不会触发，
// 必须用 onActivated/onDeactivated 管轮询启停（onActivated 首次挂载时同样会触发）。
onActivated(() => {
  void (async () => {
    // 老框架 loadSettingsFragment → startGlobalLogsPolling：日志轮询不依赖 owner。
    startGlobalLogsPolling();
    // 主题对所有角色生效且只有服务端一份真值，故 load() 不再受 owner 门限
    // （GET /api/settings 非 owner 也可读，服务端按角色脱敏绝对路径）；
    // 其余项（ffmpeg 检测、foundation 摘要、update 状态、健康轮询）仍仅 owner 需要。
    await settings.load();
    if (!isOwner.value) return;
    // 以下几段互不依赖（且都是 owner-only 端点：/api/settings/* 与 /api/update/*），
    // 并行发出；此前串在 await 链上，弱网下进页会一段段地等。
    const [updateStatus] = await Promise.all([
      settings.loadUpdateStatus(),
      loadFoundationSummary(),
      settings.refreshFfmpegPath(),
      download.refreshHealth(),
    ]);
    updateStatusLoaded.value = updateStatus;
    startHealthPolling();
  })();
});

function stopPolling() {
  stopGlobalLogsPolling();
  if (healthTimer) { clearInterval(healthTimer); healthTimer = null; }
}
onDeactivated(stopPolling);
onUnmounted(stopPolling);

// 老框架 loadSettingsFromServer 的 catch：加载失败只 toast，表单保留当前内容。
watch(() => settings.loadError, (err) => {
  if (err) toast.error(`加载设置失败：${err}`);
});

// 老框架 refreshGlobalLogs：渲染后滚动到底部——但用户往上翻看时不能拽回底部。
watch(() => settings.logs, async () => {
  await nextTick();
  autoScrollToBottomIfPinned(document.getElementById('global-logs-list'));
});

// 老框架 onFFmpegModeChange：切换检测模式时重新检测。
watch(() => settings.settings.ffmpeg_mode, () => {
  if (isOwner.value) void settings.refreshFfmpegPath();
});

async function saveSettings() {
  try {
    const message = await settings.save();
    // 老框架：优先后端 message（Aria2 重载失败警告、Windows 暂存提示等）；含警告时降级为 warning。
    if (message.includes('Aria2 未能')) toast.warn(message);
    else toast.success(message || '设置已保存');
  } catch (e: any) {
    toast.error(e?.message || '保存设置失败');
    // 老框架：409 版本冲突后重新拉取服务器设置。
    if (e?.status === 409) await settings.load();
  }
}

async function resetSettings() {
  if (!await confirmDialog({
    title: '恢复默认',
    message: '确定要恢复默认设置吗？此操作不可恢复。',
    confirmText: '恢复默认',
    tone: 'danger',
  })) return;
  try {
    const message = await settings.reset();
    toast.success(message || '已恢复默认设置');
  } catch (e: any) {
    toast.error(e?.message || '重置设置失败');
  }
}
</script>

<template>
  <section class="tab-panel">
    <div class="settings-fragment">
      <!-- Operator 会话只读提示（对齐老框架 bootstrap.js applySessionRole；viewer 无提示）。 -->
      <p v-if="auth.state.role === 'operator'" class="form-note form-note-warning operator-settings-note">
        <i class="fa-solid fa-lock"></i> 设置仅 Owner 可修改；当前为 Operator 会话，本页为只读展示。
      </p>
      <!-- 分组导航（对齐老框架 settings-group-switcher；置于 fieldset 外，Operator 禁用表单后仍可切换分组浏览）。 -->
      <div class="settings-group-switcher" role="toolbar" aria-label="设置分组">
        <button v-for="b in GROUP_BUTTONS" :key="b.key" type="button" class="btn btn-sm"
                :data-settings-group="b.key" :aria-pressed="group === b.key"
                aria-controls="settings-sections" @click="setGroup(b.key)">{{ b.label }}</button>
      </div>
      <p class="form-note settings-fragment-note">设置按用途分组；首次进入默认显示基础设置。</p>
      <fieldset :disabled="!isOwner" class="settings-sections-fieldset">
      <div class="settings-sections" id="settings-sections" @click="onSectionsClick" @keydown="onSectionsKeydown">
        <SettingsBasicSections />
        <SettingsAria2Sections />
        <SettingsDownloadSections />
        <SettingsAdvancedSections />
        <SettingsToolSections />
      </div>
      </fieldset>

      <!-- 设置操作按钮（对齐老框架 .card > .settings-actions）。 -->
      <div class="card">
        <div class="settings-actions">
          <button class="btn btn-primary" data-action="save-settings" :disabled="!isOwner || settings.saving" @click="saveSettings">
            <i class="fa-solid fa-save"></i> {{ settings.saving ? '保存中...' : '保存所有设置' }}
          </button>
          <button class="btn btn-danger" data-action="reset-settings" :disabled="!isOwner" @click="resetSettings"><i class="fa-solid fa-undo"></i> 恢复默认</button>
          <button class="btn" data-action="load-settings" :disabled="!isOwner" @click="settings.load"><i class="fa-solid fa-redo"></i> 刷新</button>
        </div>
        <!-- 第三方致谢：CC BY 4.0 许可要求保留署名（见 NOTICE.md），不可移除，
             只把它从孤立的一句英文变成有归属的中文小节。 -->
        <div class="settings-attribution">
          <div class="settings-attribution-title">第三方致谢</div>
          <ul>
            <li>GeoIP 数据来源 <a href="https://db-ip.com" target="_blank" rel="noopener noreferrer">DB-IP.com</a>（CC BY 4.0）</li>
            <li>图标来自 <a href="https://fontawesome.com" target="_blank" rel="noopener noreferrer">Font Awesome</a> Free（CC BY 4.0）</li>
          </ul>
        </div>
      </div>
    </div>
  </section>
</template>

<style scoped>
.settings-sections-fieldset {
  min-width: 0;
  margin: 0;
  padding: 0;
  border: 0;
}
.operator-settings-note { margin: 12px 0 0; }
</style>
