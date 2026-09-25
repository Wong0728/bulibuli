<script setup lang="ts">
/**
 * 设置页「Aria2 / FFmpeg」sections（从 TabSettings.vue 拆出，非 Owner 隐藏）：
 * Aria2 下载设置（含重连、高级参数、恒隐藏的 RPC 面板 bug 复刻）与 FFmpeg 设置（模式 / 检测 / 测试）。
 */
import { ref, computed } from 'vue';
import { useSettingsStore } from '@/stores/settings';
import { useAuthStore } from '@/stores/auth';
import { useToastStore } from '@/stores/toast';
import { useDownloadStore } from '@/stores/download';
import { useSettingsSections } from '@/composables/useSettingsSections';

const settings = useSettingsStore();
const auth = useAuthStore();
const toast = useToastStore();
const download = useDownloadStore();
const { collapsedSections, inGroup } = useSettingsSections();

const isOwner = computed(() => auth.state.role === 'owner');

const restartingAria2 = ref(false);
async function restartAria2() {
  restartingAria2.value = true;
  try {
    const { restarted, error, message } = await settings.restartAria2();
    if (restarted) toast.success('Aria2 已重新连接');
    else toast.error(error || message || 'Aria2 重新连接失败', 5000);
  } catch (e: any) {
    toast.error(e?.message || 'Aria2 重新连接失败', 5000);
  } finally {
    restartingAria2.value = false;
  }
}

/** aria2 指示点点击诊断：与看板页（TabHistory）同一入口，复用 store 的 diagnoseAria2，
 *  未做过健康探测时静默返回，不弹无依据的结论。 */
function onAria2DotClick() {
  const d = download.diagnoseAria2();
  if (!d) return;
  if (d.type === 'success') toast.success(d.message, d.duration);
  else if (d.type === 'info') toast.info(d.message, d.duration);
  else toast.error(d.message, d.duration);
}

/** 下载模式说明文字：老框架 onDownloadModeChange 的 switch 只处理 embedded/rpc，
 *  现有选项 embedded/external 均落在"内置 aria2c"文案上（复刻该行为，不展示 RPC 提示）。 */
const downloadModeNote = computed(() => '自动启动内置 aria2c 并通过 RPC 控制，无需手动配置');

// --- FFmpeg 检测展示（对齐老框架 refreshFFmpegDetectedPath 的渲染）---
const ffmpegModeNote = computed(() => {
  // 老框架 onFFmpegModeChange 的文案表。
  switch (settings.settings.ffmpeg_mode) {
    case 'system': return '使用系统 PATH 环境变量中找到的 ffmpeg';
    case 'embedded': return '使用程序 resources 目录内置的 ffmpeg';
    case 'custom': return '使用手动指定的自定义路径';
    default: return '自动检测：自定义路径 > 内置 ffmpeg > 环境变量 > 系统 PATH';
  }
});
const ffmpegSourceText = computed(() => {
  switch (settings.ffmpegInfo?.source) {
    case 'system': return '（系统 PATH）';
    case 'embedded': return '（内置版本）';
    case 'custom': return '（自定义路径）';
    default: return '';
  }
});
const ffmpegSourceIcon = computed(() => {
  switch (settings.ffmpegInfo?.source) {
    case 'system': return 'fa-desktop';
    case 'embedded': return 'fa-box';
    case 'custom': return 'fa-user';
    default: return 'fa-question-circle';
  }
});
/** 老框架 resultEl.innerHTML 的结构化等价：图标 + 色调 + 文案。 */
type ResultHint = { tone: string; icon: string; spin?: boolean; text: string };
const ffmpegTestResult = ref<ResultHint | null>(null);
async function onTestFfmpeg() {
  ffmpegTestResult.value = { tone: '', icon: 'fa-spinner', spin: true, text: '测试中...' };
  try {
    const { data, message } = await settings.testFFmpeg({
      mode: settings.settings.ffmpeg_mode,
      custom_path: settings.settings.ffmpeg_custom_path,
    });
    if (data?.available) {
      const sourceText = data.source === 'system' ? ' [系统PATH]'
        : data.source === 'embedded' ? ' [内置]'
        : data.source === 'custom' ? ' [自定义]' : '';
      ffmpegTestResult.value = {
        tone: 'status-success', icon: 'fa-check-circle',
        text: `FFmpeg 可用${sourceText} ${data.version ? `(${data.version})` : ''}`.trimEnd(),
      };
    } else {
      ffmpegTestResult.value = { tone: 'status-error', icon: 'fa-times-circle', text: message || 'FFmpeg 不可用' };
    }
  } catch {
    ffmpegTestResult.value = { tone: 'status-error', icon: 'fa-times-circle', text: '测试失败' };
  }
}
</script>

<template>
  <!-- Aria2（非 Owner 隐藏，对齐老框架 applySessionRole） -->
  <div v-if="isOwner" v-show="inGroup('aria2')" :class="{ collapsed: collapsedSections.has('aria2') }" class="section-collapsible" data-section="aria2">
    <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-download"></i> Aria2 下载设置</h2></div>
    <div class="section-body">
      <div class="settings-grid">
        <div class="form-group form-full">
          <label for="setting-download-mode">下载模式</label>
          <select id="setting-download-mode" class="form-control"
                  :value="settings.settings.aria2_mode ?? 'embedded'"
                  @change="settings.update({ aria2_mode: ($event.target as HTMLSelectElement).value as any })">
            <option value="embedded">内置 aria2c（推荐）</option>
            <option value="external">RPC 远程连接</option>
          </select>
          <div class="form-note" id="download-mode-note">{{ downloadModeNote }}</div>
        </div>
        <div class="form-group form-full">
          <div class="aria2-settings-status">
            <span><span class="aria2-dot" :class="download.aria2DotClass" data-aria2-status :title="download.aria2Title" role="button" tabindex="0" @click="onAria2DotClick" @keydown.enter.prevent="onAria2DotClick" @keydown.space.prevent="onAria2DotClick"></span> 保存连接设置后会立即应用，无需重启应用。</span>
            <button type="button" class="btn btn-sm" data-action="restart-aria2" :disabled="restartingAria2" @click="restartAria2">
              <i class="fa-solid fa-rotate"></i> {{ restartingAria2 ? '正在重连' : '重新连接 Aria2' }}
            </button>
          </div>
        </div>
      </div>

      <!-- 老框架 bug 复刻：onDownloadModeChange 用 `mode !== 'rpc'` 判断而实际选项值是
           'embedded'/'external'，该面板恒隐藏；按基准原则保持恒隐藏，不"顺手修掉"。 -->
      <div id="rpc-settings-panel" class="rpc-settings-panel" hidden>
        <div class="settings-grid">
          <div class="form-group">
            <label for="setting-aria2-host">主机地址</label>
            <input type="text" id="setting-aria2-host" class="form-control"
                   :value="settings.settings.aria2_host || '127.0.0.1'"
                   @change="settings.update({ aria2_host: ($event.target as HTMLInputElement).value })" />
            <div class="form-note">Aria2 RPC 主机地址</div>
          </div>
          <div class="form-group">
            <label for="setting-aria2-port">RPC 端口</label>
            <input type="number" id="setting-aria2-port" class="form-control" min="1" max="65535"
                   :value="settings.settings.aria2_port ?? 6800"
                   @change="settings.update({ aria2_port: Number(($event.target as HTMLInputElement).value) })" />
            <div class="form-note">Aria2 RPC 端口</div>
          </div>
          <div class="form-group">
            <label for="setting-aria2-secret">RPC 密钥</label>
            <input type="password" id="setting-aria2-secret" class="form-control"
                   :value="settings.settings.aria2_secret || ''"
                   @change="settings.update({ aria2_secret: ($event.target as HTMLInputElement).value })" />
            <div class="form-note">Aria2 RPC 密钥（如果有）</div>
          </div>
        </div>
      </div>

      <div class="rpc-settings-panel">
        <h3 class="settings-section-title"><i class="fa-solid fa-rocket"></i> Aria2c 高级参数</h3>
        <div class="settings-grid">
          <div class="form-group">
            <label for="setting-max-conn-per-server">每服务器最大连接数</label>
            <input type="number" id="setting-max-conn-per-server" class="form-control" min="1" max="16"
                   :value="settings.settings.aria2_max_conn_per_server ?? 16"
                   @change="settings.update({ aria2_max_conn_per_server: Number(($event.target as HTMLInputElement).value) })" />
            <div class="form-note">范围: 1-16，建议 16</div>
          </div>
          <div class="form-group">
            <label for="setting-split">分片数量</label>
            <input type="number" id="setting-split" class="form-control" min="1" max="16"
                   :value="settings.settings.aria2_split ?? 16"
                   @change="settings.update({ aria2_split: Number(($event.target as HTMLInputElement).value) })" />
            <div class="form-note">范围: 1-16，建议与连接数相同</div>
          </div>
          <div class="form-group">
            <label for="setting-min-split-size">最小分片大小</label>
            <select id="setting-min-split-size" class="form-control" :value="settings.settings.aria2_min_split_size || '10M'"
                    @change="settings.update({ aria2_min_split_size: ($event.target as HTMLSelectElement).value })">
              <option value="1M">1 MB</option><option value="5M">5 MB</option><option value="10M">10 MB</option>
            </select>
            <div class="form-note">小文件可增大</div>
          </div>
          <div class="form-group">
            <label for="setting-max-tries">最大重试次数</label>
            <input type="number" id="setting-max-tries" class="form-control" min="3" max="10"
                   :value="settings.settings.aria2_max_tries ?? 5"
                   @change="settings.update({ aria2_max_tries: Number(($event.target as HTMLInputElement).value) })" />
            <div class="form-note">范围: 3-10，建议 5</div>
          </div>
          <div class="form-group">
            <label for="setting-retry-wait">重试等待时间 (秒)</label>
            <input type="number" id="setting-retry-wait" class="form-control" min="1" max="30"
                   :value="settings.settings.aria2_retry_wait ?? 5"
                   @change="settings.update({ aria2_retry_wait: Number(($event.target as HTMLInputElement).value) })" />
            <div class="form-note">范围: 1-30，建议 5</div>
          </div>
          <div class="form-group">
            <label for="setting-max-concurrent-downloads">最大同时下载数</label>
            <input type="number" id="setting-max-concurrent-downloads" class="form-control" min="1" max="32"
                   :value="settings.settings.aria2_max_concurrent_downloads ?? 3"
                   @change="settings.update({ aria2_max_concurrent_downloads: Number(($event.target as HTMLInputElement).value) })" />
            <div class="form-note">范围 1-32，与上面的并行数保持同步</div>
          </div>
        </div>
      </div>
    </div>
  </div>

  <!-- FFmpeg 设置（非 Owner 隐藏，对齐老框架 applySessionRole） -->
  <div v-if="isOwner" v-show="inGroup('ffmpeg')" :class="{ collapsed: collapsedSections.has('ffmpeg') }" class="section-collapsible" data-section="ffmpeg">
    <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-film"></i> FFmpeg 设置</h2></div>
    <div class="section-body">
      <div class="settings-grid">
        <div class="form-group form-full">
          <label for="setting-ffmpeg-mode">FFmpeg 模式</label>
          <select id="setting-ffmpeg-mode" class="form-control"
                  :value="settings.settings.ffmpeg_mode || 'auto'"
                  @change="settings.update({ ffmpeg_mode: ($event.target as HTMLSelectElement).value as any })">
            <option value="auto">自动检测（推荐）</option>
            <option value="system">系统 PATH</option>
            <option value="embedded">内置 ffmpeg.exe</option>
            <option value="custom">手动指定路径</option>
          </select>
          <div class="form-note" id="ffmpeg-mode-note">{{ ffmpegModeNote }}</div>
        </div>
        <div class="form-group form-full" id="ffmpeg-detected-path-group">
          <span class="form-label">当前检测到的路径</span>
          <div id="ffmpeg-detected-path" class="path-preview-box">
            <!-- 对齐老框架 refreshFFmpegDetectedPath：失败→检测失败；成功→状态图标 + 来源图标 + 路径 + 来源标注。 -->
            <template v-if="settings.ffmpegDetectError">检测失败</template>
            <template v-else-if="settings.ffmpegInfo">
              <i :class="['fa-solid', settings.ffmpegInfo.available ? 'fa-check-circle status-success' : 'fa-exclamation-triangle status-error']"></i>
              <i :class="['fa-solid', ffmpegSourceIcon]"></i> {{ settings.ffmpegInfo.path || '未检测到' }}
              <span v-if="ffmpegSourceText" class="status-source">{{ ffmpegSourceText }}</span>
            </template>
            <template v-else>
              <i class="fa-solid fa-spinner fa-spin"></i> 检测中...
            </template>
          </div>
        </div>
        <div class="form-group form-full" id="ffmpeg-custom-path-group" v-show="settings.settings.ffmpeg_mode === 'custom'">
          <label for="setting-ffmpeg-path">自定义 FFmpeg 路径</label>
          <input type="text" id="setting-ffmpeg-path" class="form-control"
                 placeholder="请输入 FFmpeg 完整路径"
                 :value="settings.settings.ffmpeg_custom_path || ''"
                 @change="settings.update({ ffmpeg_custom_path: ($event.target as HTMLInputElement).value })" />
          <div class="form-note">指定自定义的 FFmpeg 可执行文件路径</div>
        </div>
        <div class="form-group form-full">
          <div class="btn-group">
            <button type="button" class="btn" data-action="test-ffmpeg" @click="onTestFfmpeg">
              <i class="fa-solid fa-vial"></i> 测试 FFmpeg
            </button>
            <span id="ffmpeg-test-result" class="test-result-hint">
              <span v-if="ffmpegTestResult" :class="ffmpegTestResult.tone">
                <i class="fa-solid" :class="[ffmpegTestResult.icon, { 'fa-spin': ffmpegTestResult.spin }]"></i>
                {{ ffmpegTestResult.text }}
              </span>
            </span>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>
