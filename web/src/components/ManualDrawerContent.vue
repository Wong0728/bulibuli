<script setup lang="ts">
/**
 * 手动查询抽屉内容（老框架 renderDrawerContentForManualQuery 的独立渲染）。
 * 从 VideoDrawer.vue 拆出的纯展示子组件：数据经 props 注入，交互经 emits 上抛。
 */
import type { QualityOption } from '@/utils/drawerFormat';
import { hideImgOnError } from '@/utils/image';

defineProps<{
  /** 封面图（后端代理地址，空串不渲染）。 */
  manualPic: string;
  manualDuration: string;
  manualPubdate: string;
  manualView: string;
  qualityOptions: QualityOption[];
  selectedQn: number;
  /** 取流结果是否已返回（未返回时不置灰任何清晰度档）。 */
  urlsLoaded: boolean;
  qualityAvailable: Set<number>;
  streamBlockedMessage: string;
  pages: any[];
  selectedPages: Set<number>;
}>();

defineEmits<{
  (e: 'select-quality', qn: number): void;
  (e: 'toggle-page', index: number): void;
  (e: 'toggle-all-pages'): void;
  (e: 'start-download'): void;
  (e: 'save-to-local'): void;
  (e: 'open-bilibili'): void;
  (e: 'download-danmaku'): void;
  (e: 'download-comments'): void;
}>();
</script>

<template>
  <div class="drawer-preview">
    <img v-if="manualPic" :src="manualPic" alt="" @error="hideImgOnError" />
    <span v-if="manualDuration" class="drawer-preview-badge">{{ manualDuration }}</span>
  </div>

  <div class="drawer-info-row">
    <div class="drawer-info-item">
      <span class="drawer-info-label">发布时间</span>
      <span class="drawer-info-value">{{ manualPubdate }}</span>
    </div>
    <div class="drawer-info-item">
      <span class="drawer-info-label">播放量</span>
      <span class="drawer-info-value">{{ manualView }}</span>
    </div>
    <div class="drawer-info-item">
      <span class="drawer-info-label">状态</span>
      <span class="drawer-info-value drawer-info-muted">未下载</span>
    </div>
  </div>

  <div class="drawer-section">
    <div class="drawer-section-title">视频下载</div>
    <div class="quality-pills" id="quality-pills-container">
      <button v-for="q in qualityOptions" :key="q.qn"
              :class="['quality-pill', { active: selectedQn === q.qn, disabled: urlsLoaded && !qualityAvailable.has(q.qn) }]"
              :disabled="urlsLoaded && !qualityAvailable.has(q.qn)"
              @click="$emit('select-quality', q.qn)">
        {{ q.label }}
        <span v-if="q.tag" class="quality-pill-tag">{{ q.tag }}</span>
      </button>
    </div>
    <div v-if="streamBlockedMessage" class="drawer-pay-note">
      <i class="fa-solid fa-coins"></i>
      {{ streamBlockedMessage }}
    </div>
    <div v-if="pages.length > 1" class="drawer-pages" id="drawer-pages-section">
      <div class="drawer-pages-header">
        <span class="drawer-pages-label">分P选择（共 {{ pages.length }} 个）</span>
        <button class="btn btn-ghost btn-sm" @click="$emit('toggle-all-pages')">全选/全不选</button>
      </div>
      <div class="drawer-pages-list">
        <label v-for="(p, i) in pages" :key="p.cid" class="drawer-page-item">
          <input type="checkbox" class="drawer-page-check" :checked="selectedPages.has(i)" @change="$emit('toggle-page', i)" />
          <span class="drawer-page-index">P{{ p.page ?? i + 1 }}</span>
          <span class="drawer-page-title">{{ p.part || `P${p.page ?? i + 1}` }}</span>
        </label>
      </div>
    </div>
    <div class="drawer-actions">
      <button class="drawer-btn drawer-btn-primary" data-action="start-manual-video" data-mutating @click="$emit('start-download')">
        <i class="fa-solid fa-download"></i>
        开始下载
      </button>
      <button class="drawer-btn drawer-btn-ghost" data-action="save-manual-to-local"
              title="用服务器登录态经代理把视频流和音频流直接保存到本机，不占用服务器存储" @click="$emit('save-to-local')">
        <i class="fa-solid fa-desktop"></i>
        保存到本机
      </button>
    </div>
  </div>

  <div class="drawer-section">
    <div class="drawer-section-title">更多选项</div>
    <div class="drawer-extras">
      <button class="drawer-extra-btn" @click="$emit('open-bilibili')">
        <i class="fa-solid fa-external-link-alt"></i>
        原视频链接
      </button>
      <button class="drawer-extra-btn" data-mutating @click="$emit('download-danmaku')">
        <i class="fa-solid fa-comment-dots"></i>
        下载弹幕
      </button>
      <button class="drawer-extra-btn" data-mutating @click="$emit('download-comments')">
        <i class="fa-solid fa-comments"></i>
        下载评论
      </button>
    </div>
  </div>
</template>

<style scoped>
/* 分P选择列表（与老框架抽屉分P区视觉一致） */
.drawer-pages-list {
  display: grid;
  gap: 0.375rem;
  max-height: 13.75rem;
  overflow-y: auto;
}
.drawer-page-item {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  min-width: 0;
  padding: 0.4375rem 0.5625rem;
  border: var(--border-width) solid var(--border);
  border-radius: var(--radius-md);
  background: var(--surface);
  cursor: pointer;
}
.drawer-page-item:hover {
  border-color: var(--brand);
  background: var(--brand-soft);
}
.drawer-page-item input { flex: 0 0 auto; }
.drawer-page-index {
  flex: 0 0 auto;
  color: var(--brand);
  font-family: var(--font-mono);
  font-size: 0.75rem;
  font-weight: 600;
}
.drawer-page-title {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text-secondary);
  font-size: 0.75rem;
}
</style>
