<script setup lang="ts">
/**
 * 抽屉「弹幕与评论历史」区块（老框架弹幕/评论版本浏览器）。
 * 从 VideoDrawer.vue 拆出的展示子组件：左侧双列版本列表 + 下方内容查看器。
 * 数据经 props 注入，交互经 emits 上抛。
 */
import type { SidecarViewer } from '@/composables/videoDrawer';
import { commentUserTone, formatDanmakuTime, formatSidecarTime, locationLabel, sidecarVersionLabel } from '@/utils/drawerFormat';

defineProps<{
  danmakuVersions: any[];
  commentVersions: any[];
  sidecarViewer: SidecarViewer | null;
  sidecarLoading: boolean;
  sidecarError: string;
  canBrowserDownload: boolean;
  selectedFilePaths: Set<string>;
}>();

defineEmits<{
  (e: 'load-danmaku', path: string): void;
  (e: 'load-comments', path: string): void;
  (e: 'toggle-file', path: string): void;
}>();
</script>

<template>
  <div class="drawer-section">
    <div class="drawer-section-title">弹幕与评论历史</div>
    <div class="drawer-sidecar-browser">
      <div class="sidecar-version-column">
        <div class="sidecar-version-title"><i class="fa-solid fa-comment-dots"></i> 弹幕版本（{{ danmakuVersions.length }}）</div>
        <div class="sidecar-version-list">
          <div v-for="v in danmakuVersions" :key="String(v.path)" class="sidecar-version-row">
            <button class="sidecar-version-btn" :class="{ active: sidecarViewer?.path === v.path }" :title="v.path" @click="$emit('load-danmaku', v.path)">
              <i class="fa-solid fa-comment-dots"></i>
              <span>{{ sidecarVersionLabel(v) }}</span>
              <small>{{ locationLabel(v.location) }} · {{ String(v.format || '').toUpperCase() }}</small>
            </button>
            <label v-if="canBrowserDownload && v.path" class="drawer-file-check" title="勾选后随“下载所选”保存到本机">
              <input type="checkbox" :checked="selectedFilePaths.has(String(v.path))" @change="$emit('toggle-file', String(v.path))" />
            </label>
          </div>
          <div v-if="!danmakuVersions.length" class="drawer-comments-hint">暂无本地版本</div>
        </div>
      </div>
      <div class="sidecar-version-column">
        <div class="sidecar-version-title"><i class="fa-solid fa-comments"></i> 评论版本（{{ commentVersions.length }}）</div>
        <div class="sidecar-version-list">
          <div v-for="(f, i) in commentVersions" :key="String(f.path) + i" class="sidecar-version-row">
            <button class="sidecar-version-btn" :class="{ active: sidecarViewer?.path === f.path }" :title="f.path" @click="$emit('load-comments', f.path)">
              <i class="fa-solid fa-comments"></i>
              <span>{{ sidecarVersionLabel(f) }}</span>
              <small>{{ locationLabel(f.location) }} · {{ String(f.format || '').toUpperCase() }}</small>
            </button>
            <label v-if="canBrowserDownload && f.path" class="drawer-file-check" title="勾选后随“下载所选”保存到本机">
              <input type="checkbox" :checked="selectedFilePaths.has(String(f.path))" @change="$emit('toggle-file', String(f.path))" />
            </label>
          </div>
          <div v-if="!commentVersions.length" class="drawer-comments-hint">暂无本地版本</div>
        </div>
      </div>
    </div>
    <div class="drawer-comments" id="drawer-sidecar-viewer">
      <div v-if="sidecarLoading" class="drawer-comments-hint"><i class="fa-solid fa-spinner fa-spin"></i> 加载中...</div>
      <div v-else-if="sidecarError" class="drawer-comments-hint status-error">{{ sidecarError }}</div>
      <template v-else-if="sidecarViewer">
        <!-- 评论 -->
        <template v-if="sidecarViewer.kind === 'comments'">
          <div v-if="!sidecarViewer.exists" class="drawer-comments-hint">
            <i class="fa-solid fa-comment-slash"></i> 未下载评论（可先点下方“下载评论”，完成后再查看）
          </div>
          <template v-else-if="sidecarViewer.format === 'json' && Array.isArray(sidecarViewer.comments)">
            <div v-if="!sidecarViewer.comments.length" class="drawer-comments-hint"><i class="fa-solid fa-comment-slash"></i> 暂无评论内容</div>
            <div v-else class="drawer-comments-list">
              <article v-for="(c, i) in sidecarViewer.comments" :key="i" class="cmt-card">
                <div class="cmt-line">
                  <span :class="['cmt-user', commentUserTone(c.name_color)]">{{ c.uname || '' }}</span>
                  <span v-if="Number(c.vip_status || 0) > 0" class="cmt-vip">{{ c.vip_label || '大会员' }}</span>
                  <span class="cmt-lv">Lv{{ c.level || 0 }}</span>
                  <span class="cmt-meta"><i class="fa-solid fa-thumbs-up"></i> {{ c.like || 0 }} · <i class="fa-solid fa-comment"></i> {{ c.total_replies || 0 }} · {{ formatSidecarTime(c.ctime) }}</span>
                </div>
                <div class="cmt-text">{{ c.message || '' }}</div>
                <div v-if="Array.isArray(c.replies) && c.replies.length" class="cmt-replies">
                  <div class="cmt-replies-title">回复 · 显示 {{ c.replies.length }}/{{ c.total_replies || 0 }} 条</div>
                  <div v-for="(reply, j) in c.replies" :key="j" class="cmt-reply">
                    <div class="cmt-line">
                      <span :class="['cmt-user', commentUserTone(reply.name_color)]">{{ reply.uname || '' }}</span>
                      <span v-if="Number(reply.vip_status || 0) > 0" class="cmt-vip">{{ reply.vip_label || '大会员' }}</span>
                      <span class="cmt-lv">Lv{{ reply.level || 0 }}</span>
                      <span class="cmt-meta"><i class="fa-solid fa-thumbs-up"></i> {{ reply.like || 0 }} · {{ formatSidecarTime(reply.ctime) }}</span>
                    </div>
                    <div class="cmt-text">{{ reply.message || '' }}</div>
                  </div>
                </div>
              </article>
            </div>
          </template>
          <div v-else-if="sidecarViewer.content" class="drawer-comments-hint"><i class="fa-solid fa-file-lines"></i> 该评论版本为原始文本，前端不提供直接查看</div>
          <div v-else class="drawer-comments-hint"><i class="fa-solid fa-comment-slash"></i> 暂无评论内容</div>
        </template>
        <!-- 弹幕 -->
        <template v-else>
          <template v-if="sidecarViewer.format === 'json' && Array.isArray(sidecarViewer.danmaku)">
            <div class="drawer-sidecar-result-title">
              <span><i class="fa-solid fa-comment-dots"></i> {{ sidecarViewer.danmaku.length }} 条弹幕</span>
              <span>{{ sidecarViewer.path }}</span>
            </div>
            <div v-if="sidecarViewer.danmaku.length" class="drawer-danmaku-list">
              <div v-for="(item, i) in sidecarViewer.danmaku" :key="i" class="drawer-danmaku-row">
                <span class="drawer-danmaku-time">{{ formatDanmakuTime(item) }}</span>
                <span class="drawer-danmaku-text">{{ item.content ?? item.text ?? item.message ?? '' }}</span>
                <span v-if="item.mid_hash ?? item.user_hash" class="drawer-danmaku-user">{{ item.mid_hash ?? item.user_hash }}</span>
              </div>
            </div>
            <div v-else class="drawer-comments-hint">暂无弹幕内容</div>
          </template>
          <div v-else class="drawer-comments-hint"><i class="fa-solid fa-file-lines"></i> 该弹幕版本为原始文本，前端不提供直接查看</div>
        </template>
      </template>
      <div v-else class="drawer-comments-hint">选择上方任一弹幕或评论版本查看本地内容</div>
    </div>
  </div>
</template>
