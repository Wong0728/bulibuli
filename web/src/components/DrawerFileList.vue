<script setup lang="ts">
/**
 * 抽屉「全部产物」区块（老框架 renderDrawerFiles / renderBrowserDownloadToolbar）。
 * 从 VideoDrawer.vue 拆出的展示子组件：同时展示 manual、自动目录和历史归档，
 * 并承载「服务器到本机」勾选与烧录入口。数据经 props 注入，交互经 emits 上抛。
 */
import { TYPE_LABELS, TYPE_ICONS, fileVersionLabel, formatSize, hasSidecar, locationLabel, SIDECAR_ICONS } from '@/utils/drawerFormat';

defineProps<{
  /** 产物概览统计；无主产物时为 null（不渲染概览条）。 */
  overview: { total: number; locations: number; videos: number; archives: number } | null;
  /** 主产物文件数（弹幕/评论侧车除外），控制「服务器到本机」工具条显隐。 */
  primaryFilesCount: number;
  /** [产物区域, 文件列表] 分组。 */
  fileGroups: [string, any[]][];
  /** 对齐老框架 renderDrawerFiles 空态分支：无主产物但有弹幕/评论文件时提示从下方入口查看。 */
  hasSidecarFiles: boolean;
  /** 兜底：老框架 sidecar 四态图标（视频/弹幕/评论/字幕 ✓/—）。 */
  sidecar: Record<string, any> | null;
  canOpenDirectory: boolean;
  canBrowserDownload: boolean;
  selectMode: boolean;
  selectedFilePaths: Set<string>;
  burned: Record<string, any>;
  canBurn: boolean;
  /** 进行中的烧录来源（danmaku/subtitle），null 表示空闲。 */
  burningTask: string | null;
}>();

defineEmits<{
  (e: 'toggle-select-mode', enabled: boolean): void;
  (e: 'download-selected'): void;
  (e: 'toggle-file', path: string): void;
  (e: 'copy-path', path: string): void;
  (e: 'open-directory', path: string): void;
  (e: 'burn', source: 'danmaku' | 'subtitle'): void;
}>();
</script>

<template>
  <div class="drawer-section">
    <div class="drawer-section-title">全部产物</div>
    <div v-if="overview" class="drawer-artifact-overview">
      <span><strong>{{ overview.total }}</strong> 个文件</span>
      <span><strong>{{ overview.locations }}</strong> 个产物区域</span>
      <span><strong>{{ overview.videos }}</strong> 个视频产物</span>
      <span><strong>{{ overview.archives }}</strong> 个历史归档文件</span>
    </div>
    <!-- 服务器到本机：勾选产物后经浏览器保存（对齐老框架 renderBrowserDownloadToolbar） -->
    <div v-if="canBrowserDownload && primaryFilesCount" class="drawer-browser-download-bar">
      <label class="drawer-browser-master" title="勾选后可挑选下方要保存到本机的产物，默认全选">
        <input type="checkbox" :checked="selectMode" @change="$emit('toggle-select-mode', ($event.target as HTMLInputElement).checked)" />
        <span><i class="fa-solid fa-desktop"></i> 服务器到本机</span>
      </label>
      <button class="btn btn-sm btn-primary" id="drawer-browser-download-btn" :disabled="selectedFilePaths.size === 0" :hidden="!selectMode"
              title="把勾选的产物通过浏览器保存到本机" @click="$emit('download-selected')">
        <i class="fa-solid fa-download"></i> 下载所选（{{ selectedFilePaths.size }}）
      </button>
    </div>
    <div class="drawer-file-list" id="drawer-file-list">
      <template v-if="fileGroups.length">
        <div v-for="[location, entries] in fileGroups" :key="location" class="drawer-file-group">
          <div class="drawer-file-group-title">
            <span><i class="fa-solid fa-folder"></i> {{ locationLabel(location) }}</span>
            <span>{{ entries.length }} 个文件</span>
          </div>
          <div v-for="(f, i) in entries" :key="String(f.path) + i" class="drawer-file-item" :data-file-type="f.file_type || f.type || 'other'">
            <div class="drawer-file-icon"><i class="fa-solid" :class="TYPE_ICONS[f.file_type || f.type || 'other'] || 'fa-file'"></i></div>
            <div class="drawer-file-main">
              <div class="drawer-file-name" :title="f.display_path || f.name">{{ f.name || '未知文件' }}</div>
              <div class="drawer-file-meta">
                <span class="drawer-file-type">{{ TYPE_LABELS[f.file_type || f.type || 'other'] || '文件' }}</span>
                <span class="drawer-file-version" :class="{ current: f.is_current }">{{ fileVersionLabel(f) }}</span>
                <span v-if="f.size" class="drawer-file-size">{{ formatSize(Number(f.size)) }}</span>
                <span v-if="f.format" class="drawer-file-format">{{ f.format }}</span>
                <span v-if="f.modified_at">{{ new Date(Number(f.modified_at) * 1000).toLocaleString() }}</span>
              </div>
              <div v-if="f.display_path" class="drawer-file-display-path" :title="f.display_path">{{ f.display_path }}</div>
            </div>
            <div class="drawer-file-actions">
              <label v-if="canBrowserDownload && f.path" class="drawer-file-check" title="勾选后随“下载所选”保存到本机">
                <input type="checkbox" :checked="selectedFilePaths.has(String(f.path))" @change="$emit('toggle-file', String(f.path))" />
              </label>
              <button v-if="f.display_path" class="btn btn-sm btn-ghost" title="复制文件路径" @click="$emit('copy-path', f.display_path)">
                <i class="fa-solid fa-copy"></i> 路径
              </button>
              <button v-if="canOpenDirectory && f.path" class="btn btn-sm btn-ghost" title="打开所在目录" @click="$emit('open-directory', f.path)">
                <i class="fa-solid fa-folder-open"></i> 打开
              </button>
              <template v-if="(f.file_type || f.type) === 'video'">
                <span v-if="burned.danmaku" class="burn-badge burned" title="弹幕已烧录"><i class="fa-solid fa-check"></i> 弹幕已烧录</span>
                <span v-if="burned.subtitle" class="burn-badge burned" title="字幕已烧录"><i class="fa-solid fa-check"></i> 字幕已烧录</span>
                <button v-if="!burned.danmaku" class="btn btn-sm btn-primary" data-mutating
                        :disabled="!canBurn || burningTask !== null"
                        :title="canBurn ? '将弹幕烧录进视频' : '当前 FFmpeg 不支持烧录（精简版缺少 ass 滤镜或视频编码器）。建议下载完整版 FFmpeg，或在 设置 → FFmpeg 中更换自定义路径'"
                        @click="$emit('burn', 'danmaku')">
                  <i class="fa-solid fa-fire"></i> {{ burningTask === 'danmaku' ? '烧录中…' : '烧录弹幕' }}
                </button>
                <button v-if="!burned.subtitle" class="btn btn-sm btn-primary" data-mutating
                        :disabled="!canBurn || burningTask !== null"
                        :title="canBurn ? '将 CC 字幕烧录进视频' : '当前 FFmpeg 不支持烧录（精简版缺少 ass 滤镜或视频编码器）。建议下载完整版 FFmpeg，或在 设置 → FFmpeg 中更换自定义路径'"
                        @click="$emit('burn', 'subtitle')">
                  <i class="fa-solid fa-closed-captioning"></i> {{ burningTask === 'subtitle' ? '烧录中…' : '烧录字幕' }}
                </button>
              </template>
            </div>
          </div>
        </div>
      </template>
      <div v-else-if="hasSidecarFiles" class="drawer-files-empty"><i class="fa-solid fa-layer-group"></i>弹幕与评论版本请从下方入口查看</div>
      <div v-else-if="sidecar" class="drawer-sidecar">
        <span v-for="it in SIDECAR_ICONS" :key="it.key" class="sidecar-icon" :class="hasSidecar(sidecar[it.key]) ? 'ok' : 'missing'"
              :title="`${it.label}: ${hasSidecar(sidecar[it.key]) ? '已下载' : '未下载'}`">
          <i class="fa-solid" :class="it.icon"></i>{{ hasSidecar(sidecar[it.key]) ? '✓' : '—' }}
        </span>
      </div>
      <div v-else class="drawer-files-empty"><i class="fa-solid fa-inbox"></i> 暂无本地文件记录</div>
    </div>
  </div>
</template>
