<script setup lang="ts">
/**
 * 设置页「工具 / 安全」分组 sections（从 TabSettings.vue 拆出）：
 * 全局日志（15s 自动轮询 + 手动刷新）与本机配置（只读文档 + 基础配置摘要）。
 */
import { computed } from 'vue';
import { useSettingsStore } from '@/stores/settings';
import { useAuthStore } from '@/stores/auth';
import { useSettingsSections } from '@/composables/useSettingsSections';
import { useGlobalLogs } from '@/composables/useGlobalLogs';
import { useFoundationSummary } from '@/composables/useFoundationSummary';

const settings = useSettingsStore();
const auth = useAuthStore();
const { collapsedSections, inGroup } = useSettingsSections();
const { refreshGlobalLogs } = useGlobalLogs();
const { foundationStatus, foundationError, foundationModeName, copyAiSkillPath } = useFoundationSummary();

const isOwner = computed(() => auth.state.role === 'owner');

/** 手动刷新全局日志（对齐老框架 data-action="refresh-global-logs" 按钮，复用轮询防抖）。 */
function loadRecentLogs() { void refreshGlobalLogs(); }

/** 老框架渲染用后端 time 字符串（'--:--:--' 兜底）+ [uid] 标签。 */
const logRows = computed(() => settings.logs.map(l => ({
  time: (l as any).time || '--:--:--',
  level: l.level || 'info',
  message: l.message,
  uid: l.uid,
})));
</script>

<template>
  <!-- 全局日志（对齐老框架 logs section：15 秒自动轮询 + 手动刷新）。 -->
  <div v-show="inGroup('logs')" :class="{ collapsed: collapsedSections.has('logs') }" class="section-collapsible" data-section="logs">
    <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-terminal"></i> 全局日志</h2></div>
    <div class="section-body">
      <div class="form-group form-full">
        <div class="btn-group">
          <button type="button" class="btn btn-sm" data-action="refresh-global-logs" @click="loadRecentLogs">
            <i class="fa-solid fa-rotate"></i> 刷新
          </button>
          <span class="form-note" id="global-logs-hint">最近 100 条监控日志（跨博主），每 15 秒自动刷新</span>
        </div>
        <div v-if="settings.logsError" class="live-alert error" role="alert" id="global-logs-error">
          日志加载失败，请重试：{{ settings.logsError }}
        </div>
        <div id="global-logs-list" class="blogger-logs-panel">
          <div v-if="logRows.length === 0 && !settings.logsError" class="empty-state empty-state-padded"><i class="fa-solid fa-inbox"></i><p>暂无日志</p></div>
          <div v-for="(l, i) in logRows" v-else :key="i" :class="['log-entry', `log-level-${l.level}`]">
            <span class="log-time">{{ l.time }}</span>
            <span v-if="l.uid" class="log-uid">[{{ l.uid }}]</span>
            <span>{{ l.message }}</span>
          </div>
        </div>
      </div>
    </div>
  </div>

  <!-- 本机配置（对齐老框架 local-config section：只读文档；实际编辑入口仅在本机 TUI / SSH 隧道）。非 owner 隐藏。 -->
  <div v-if="isOwner" v-show="inGroup('local-config')" :class="{ collapsed: collapsedSections.has('local-config') }" class="section-collapsible" data-section="local-config">
    <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-shield-halved"></i> 本机配置</h2></div>
    <div class="section-body">
      <p class="form-note">以下配置项涉及本机安全或网络绑定，仅允许在服务端本地操作，远程 Web 无法修改。</p>
      <ol class="settings-local-docs">
        <li><strong>基础网络 / 访问模式</strong>：在服务器终端 TUI 输入 <code>setup</code> 进入设置向导，可切换仅本机 / 局域网 / 反向代理模式。</li>
        <li><strong>AI Skill</strong>：同一向导中开关。启用后复制下方路径发给 AI 即可使用。</li>
        <li><strong>Cookie 信任 / FFmpeg 白名单</strong>：首次使用时 TUI 会交互式提示确认，无需手动编辑配置文件。</li>
      </ol>
      <div id="ai-skill-path-box" class="form-group form-full settings-ai-skill-path" v-show="foundationStatus?.ai_skill_enabled">
        <span class="form-label">AI Skill 文件路径</span>
        <div class="path-preview-box settings-path-preview">
          <code id="ai-skill-path-text">{{ foundationStatus?.ai_skill_path || '正在获取路径...' }}</code>
          <button class="btn btn-sm" id="copy-ai-skill-path-btn" @click="copyAiSkillPath"><i class="fa-solid fa-copy"></i> 复制路径</button>
        </div>
        <div class="form-note settings-ai-skill-note">复制路径 → 发给 AI → 让它阅读该文件并学习如何使用</div>
      </div>
      <div id="foundation-summary-content" class="form-group form-full settings-foundation-summary">
        <p v-if="foundationError" class="form-note">基础配置状态读取失败：{{ foundationError }}</p>
        <p v-else-if="!foundationStatus" class="form-note">正在读取基础配置状态...</p>
        <template v-else>
          <p class="form-note">基础配置状态：{{ foundationStatus.configuration_status === 'normal' ? '正常' : '需要检查' }}</p>
          <p class="form-note">AI Skill：{{ foundationStatus.ai_skill_enabled ? '已启用' : '已关闭' }}</p>
          <p class="form-note">当前访问模式：{{ foundationModeName }}</p>
          <p class="form-note">基础配置入口：{{ foundationStatus.setup_access === 'unavailable' ? '未启动' : '仅本机可访问（127.0.0.1）' }}</p>
          <p v-if="foundationStatus.restart_required" class="form-note form-note-warning">基础网络配置已保存但尚未生效；请重启程序。</p>
        </template>
      </div>
    </div>
  </div>
</template>
