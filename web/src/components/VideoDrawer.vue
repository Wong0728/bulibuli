<script setup lang="ts">
/**
 * 视频详情侧拉抽屉（单列布局，对齐老框架 renderDrawerContent / renderDrawerContentForManualQuery）。
 *
 * 数据与交互全部收敛在 useVideoDrawer() 单例 composable（见 composables/videoDrawer.ts），
 * 本组件只负责抽屉骨架与各区块的组装：
 * - 手动查询抽屉 → ManualDrawerContent.vue（老框架 renderDrawerContentForManualQuery 独立渲染）；
 * - 已下载抽屉 → DrawerFileList.vue（全部产物）+ DrawerSidecarBrowser.vue（弹幕/评论历史）。
 * - DOM 常驻 + .active class 驱动 transition（与老框架动效一致）。
 */
import { ref, toRef, onUnmounted } from 'vue';
import { drawerState, openDrawer, closeDrawer } from '@/composables/drawer';
import { useModalFocus } from '@/composables/modalFocus';
import { useVideoDrawer } from '@/composables/videoDrawer';
import { video as videoApi } from '@/api';
import { hideImgOnError } from '@/utils/image';
import { QUALITY_OPTIONS, formatDuration, formatSize, formatSpeed, formatViewCount } from '@/utils/drawerFormat';
import ManualDrawerContent from '@/components/ManualDrawerContent.vue';
import DrawerFileList from '@/components/DrawerFileList.vue';
import DrawerSidecarBrowser from '@/components/DrawerSidecarBrowser.vue';

const drawerRoot = ref<HTMLElement | null>(null);
useModalFocus(toRef(drawerState, 'visible'), drawerRoot, closeDrawer);

const d = useVideoDrawer();
onUnmounted(() => d.stopBurnPolling());
</script>

<template>
  <!-- DOM 常驻：进出场动画由 .active class 驱动（与老框架一致） -->
  <div class="drawer-overlay" :class="{ active: drawerState.visible }" id="drawer-overlay" data-action="close-video-drawer" @click="closeDrawer"></div>
  <div ref="drawerRoot" class="video-drawer" :class="{ active: drawerState.visible, 'browser-select-on': d.selectMode.value }" id="video-drawer" role="dialog" aria-modal="true" aria-labelledby="drawer-video-title">
    <div class="drawer-header">
      <div class="drawer-title" id="drawer-video-title" :title="d.drawerTitle.value">{{ d.detailLoading.value ? '加载中...' : d.drawerTitle.value }}</div>
      <button class="drawer-close-btn" data-action="close-video-drawer" aria-label="关闭" @click="closeDrawer">
        <i class="fa-solid fa-times"></i>
      </button>
    </div>

    <div class="drawer-body" id="drawer-body">
      <!-- 手动查询抽屉：独立渲染（老框架 renderDrawerContentForManualQuery），不套本地详情 tab -->
      <ManualDrawerContent v-if="d.isManual.value"
                           :manual-pic="d.manualPic.value"
                           :manual-duration="d.manualDuration.value"
                           :manual-pubdate="d.manualPubdate.value"
                           :manual-view="d.manualView.value"
                           :quality-options="QUALITY_OPTIONS"
                           :selected-qn="d.selectedQn.value"
                           :urls-loaded="!!d.urls.value"
                           :quality-available="d.qualityAvailable.value"
                           :stream-blocked-message="d.streamBlockedMessage.value"
                           :pages="d.pages.value"
                           :selected-pages="d.selectedPages.value"
                           @select-quality="d.selectQuality"
                           @toggle-page="d.togglePage"
                           @toggle-all-pages="d.toggleAllPages"
                           @start-download="d.startOrRetryDownload"
                           @save-to-local="d.saveManualToLocal"
                           @open-bilibili="d.openBilibili"
                           @download-danmaku="d.downloadDanmakuNow"
                           @download-comments="d.downloadCommentsNow" />

      <!-- 已下载/历史抽屉：单列布局（对齐老框架 renderDrawerContent，无 tab） -->
      <template v-else>
        <div v-if="d.detailLoading.value" class="drawer-logs-hint"><i class="fa-solid fa-spinner fa-spin"></i> 正在加载视频详情…</div>
        <div v-else-if="!d.video.value" class="drawer-empty-state">
          <i class="fa-solid fa-exclamation-circle drawer-error-icon"></i>
          <p class="drawer-empty-message">未找到该视频记录</p>
          <p class="empty-hint">该视频可能尚未入库或已被删除</p>
        </div>
        <template v-else>
          <div class="drawer-preview">
            <img :src="d.coverUrl.value" alt="" @error="hideImgOnError" />
            <span v-if="d.video.value.duration" class="drawer-preview-badge">{{ formatDuration(Number(d.video.value.duration)) }}</span>
            <span class="drawer-state-badge" :class="`state-${d.stateDot.value}`">{{ d.stateLabel.value }}</span>
          </div>

          <div v-if="d.video.value.reupload_of" class="drawer-reupload-hint">
            <i class="fa-solid fa-exclamation-triangle"></i>
            可能是 <a href="javascript:void(0)" @click="openDrawer({ bvid: d.video.value.reupload_of })">{{ d.video.value.reupload_of }}</a> 的重传
          </div>

          <div class="drawer-info-row">
            <div class="drawer-info-item">
              <span class="drawer-info-label">发布时间</span>
              <span class="drawer-info-value">{{ d.pubDate.value || '--' }}</span>
            </div>
            <div class="drawer-info-item">
              <span class="drawer-info-label">播放量</span>
              <span class="drawer-info-value">{{ d.video.value.view != null ? formatViewCount(Number(d.video.value.view)) : '--' }}</span>
            </div>
            <div class="drawer-info-item">
              <span class="drawer-info-label">下载时间</span>
              <span class="drawer-info-value">{{ d.video.value.download_time || '--' }}</span>
            </div>
            <div v-if="d.video.value.md5" class="drawer-info-item">
              <span class="drawer-info-label">MD5</span>
              <span class="drawer-info-value drawer-md5" :title="d.video.value.md5">{{ String(d.video.value.md5).slice(0, 16) }}...</span>
              <span v-if="d.video.value.md5_last_checked_at" class="drawer-info-sub">校验于 {{ new Date(d.video.value.md5_last_checked_at).toLocaleString() }}</span>
            </div>
          </div>

          <div v-if="d.video.value.blogger" class="drawer-blogger">
            <img v-if="d.video.value.blogger.face" :src="videoApi.proxyImage(d.video.value.blogger.face)" class="drawer-blogger-avatar" alt=""
                 @error="hideImgOnError" />
            <div v-else class="drawer-blogger-avatar blogger-avatar-placeholder"><i class="fa-solid fa-user"></i></div>
            <div class="drawer-blogger-info">
              <div class="drawer-blogger-name">{{ d.video.value.blogger.name || d.video.value.blogger.uid || '--' }}</div>
              <div class="drawer-blogger-uid">UID: {{ d.video.value.blogger.uid || '--' }}</div>
            </div>
          </div>

          <div v-if="d.state.value === 'pay_blocked' && d.video.value.pay_note" class="drawer-pay-note">
            <i class="fa-solid fa-coins"></i>
            {{ String(d.video.value.pay_note).endsWith('_paid') ? '充电专属（当前账号可下载）' : '充电专属（当前账号不可下载）' }}
            <span class="drawer-info-sub">{{ d.video.value.pay_note }}</span>
          </div>

          <div v-if="d.filePath.value || d.relativePath.value" class="drawer-file-path" :title="d.filePath.value || d.relativePath.value">
            <i class="fa-solid fa-file-video"></i>
            <span>{{ d.filePath.value || d.relativePath.value }}</span>
            <button v-if="d.filePath.value" class="btn btn-sm btn-ghost" title="复制路径" @click="d.copyToClipboard(d.filePath.value)"><i class="fa-solid fa-copy"></i></button>
            <button v-if="d.canOpenDirectory.value && d.relativePath.value" class="btn btn-sm btn-ghost" title="打开文件所在目录" @click="d.openDirectoryTop"><i class="fa-solid fa-folder-open"></i></button>
          </div>

          <!-- 进度条（活跃状态显示：下载中 / 等待 / 已暂停） -->
          <template v-if="d.taskActive.value">
            <div class="drawer-progress">
              <progress class="drawer-progress-bar" max="100" :value="d.progressPercent.value"></progress>
            </div>
            <div class="drawer-progress-text">
              <span>{{ d.isPaused.value ? `已暂停 ${d.progressPercent.value}%` : `${d.progressPercent.value}%` }}</span>
              <span v-if="!d.isPaused.value && d.task.value?.speed">{{ formatSpeed(d.task.value.speed) }}</span>
              <span v-if="d.task.value?.downloaded_size && d.task.value?.total_size">{{ formatSize(d.task.value.downloaded_size) }} / {{ formatSize(d.task.value.total_size) }}</span>
            </div>
          </template>

          <!-- 全部产物：同时展示 manual、自动目录和历史归档。 -->
          <DrawerFileList :overview="d.overview.value"
                          :primary-files-count="d.primaryFiles.value.length"
                          :file-groups="d.fileGroups.value"
                          :has-sidecar-files="d.hasSidecarFiles.value"
                          :sidecar="d.sidecar.value"
                          :can-open-directory="d.canOpenDirectory.value"
                          :can-browser-download="d.canBrowserDownload.value"
                          :select-mode="d.selectMode.value"
                          :selected-file-paths="d.selectedFilePaths.value"
                          :burned="d.burned.value"
                          :can-burn="d.canBurn.value"
                          :burning-task="d.burningTask.value"
                          @toggle-select-mode="d.toggleSelectMode"
                          @download-selected="d.browserDownloadSelected"
                          @toggle-file="d.toggleFile"
                          @copy-path="d.copyToClipboard"
                          @open-directory="d.openFileDirectory"
                          @burn="d.burnMedia" />

          <!-- 弹幕与评论历史浏览器。 -->
          <DrawerSidecarBrowser :danmaku-versions="d.danmakuVersions.value"
                                :comment-versions="d.commentVersions.value"
                                :sidecar-viewer="d.sidecarViewer.value"
                                :sidecar-loading="d.sidecarLoading.value"
                                :sidecar-error="d.sidecarError.value"
                                :can-browser-download="d.canBrowserDownload.value"
                                :selected-file-paths="d.selectedFilePaths.value"
                                @load-danmaku="d.loadDanmakuViewer"
                                @load-comments="d.loadCommentsViewer"
                                @toggle-file="d.toggleFile" />

          <!-- 实时数据：按需加载，避免打开抽屉即触发风控 -->
          <div class="drawer-section">
            <div class="drawer-section-title">
              实时数据
              <button class="btn btn-sm btn-ghost" :disabled="d.liveStatsLoading.value" title="从 B 站拉取最新数据（5 分钟缓存）" @click="d.loadVideoInfo(false)">
                <i class="fa-solid fa-sync-alt" :class="{ 'fa-spin': d.liveStatsLoading.value }"></i> 刷新数据
              </button>
            </div>
            <div class="drawer-live-stats">
              <div v-if="!d.liveStatsLoaded.value" class="drawer-live-stats-hint">点击"刷新数据"从 B 站拉取最新统计（点赞 / 投币 / 收藏 / 评论等）</div>
              <template v-else-if="d.videoInfo.value?.stat">
                <div class="drawer-live-stats-grid">
                  <div class="drawer-stat-cell"><span class="drawer-stat-label">播放</span><span class="drawer-stat-value">{{ formatViewCount(d.videoInfo.value.stat.view) }}</span></div>
                  <div class="drawer-stat-cell"><span class="drawer-stat-label">弹幕</span><span class="drawer-stat-value">{{ formatViewCount(d.videoInfo.value.stat.danmaku) }}</span></div>
                  <div class="drawer-stat-cell"><span class="drawer-stat-label">评论</span><span class="drawer-stat-value">{{ formatViewCount(d.videoInfo.value.stat.reply) }}</span></div>
                  <div class="drawer-stat-cell"><span class="drawer-stat-label">收藏</span><span class="drawer-stat-value">{{ formatViewCount(d.videoInfo.value.stat.favorite) }}</span></div>
                  <div class="drawer-stat-cell"><span class="drawer-stat-label">投币</span><span class="drawer-stat-value">{{ formatViewCount(d.videoInfo.value.stat.coin) }}</span></div>
                  <div class="drawer-stat-cell"><span class="drawer-stat-label">分享</span><span class="drawer-stat-value">{{ formatViewCount(d.videoInfo.value.stat.share) }}</span></div>
                  <div class="drawer-stat-cell"><span class="drawer-stat-label">点赞</span><span class="drawer-stat-value">{{ formatViewCount(d.videoInfo.value.stat.like) }}</span></div>
                </div>
                <div class="drawer-live-owner">
                  <span>UP 主：{{ d.videoInfo.value.owner?.name || '--' }}</span>
                  <span class="drawer-info-sub">MID: {{ d.videoInfo.value.owner?.mid ?? '--' }}</span>
                </div>
              </template>
              <div v-else class="drawer-live-stats-hint">实时数据暂不可用</div>
            </div>
          </div>

          <!-- 主操作。 -->
          <div class="drawer-section">
            <div class="drawer-section-title">操作</div>
            <div class="drawer-actions">
              <button v-if="d.taskStatus.value === 'downloading' || d.taskStatus.value === 'pending' || d.taskStatus.value === 'retrying'" class="drawer-btn drawer-btn-primary" disabled>
                <i class="fa-solid fa-spinner fa-spin"></i> {{ d.taskStatus.value === 'retrying' ? '重试中' : '下载中' }} {{ d.progressPercent.value }}%
              </button>
              <button v-else-if="d.isPaused.value" class="drawer-btn drawer-btn-primary" disabled>
                <i class="fa-solid fa-pause"></i> 已暂停 {{ d.progressPercent.value }}%
              </button>
              <button v-else-if="d.state.value === 'completed' || d.state.value === 'tampered'" class="drawer-btn drawer-btn-success" disabled>
                <i class="fa-solid fa-check-circle"></i> 已完成
              </button>
              <button v-else-if="d.canRetry.value" class="drawer-btn drawer-btn-primary" data-mutating @click="d.startOrRetryDownload">
                <i class="fa-solid fa-redo"></i> 重新下载
              </button>
              <button v-else class="drawer-btn drawer-btn-primary" data-mutating @click="d.startOrRetryDownload">
                <i class="fa-solid fa-download"></i> 开始下载
              </button>
              <button v-if="d.taskId.value && d.taskActive.value" class="drawer-btn drawer-btn-ghost" data-mutating @click="d.isPaused.value ? d.resumeTask() : d.pauseTask()">
                <i class="fa-solid" :class="d.isPaused.value ? 'fa-play' : 'fa-pause'"></i> {{ d.isPaused.value ? '恢复下载' : '暂停下载' }}
              </button>
              <button class="drawer-btn drawer-btn-danger" data-mutating @click="d.deleteRecord">
                <i class="fa-solid fa-trash"></i> 删除记录
              </button>
            </div>
            <div class="drawer-extras">
              <button class="drawer-extra-btn" data-mutating @click="d.downloadCoverNow">
                <i class="fa-solid fa-image"></i> 下载封面
              </button>
              <button class="drawer-extra-btn" @click="d.openBilibili">
                <i class="fa-solid fa-external-link-alt"></i> 原视频链接
              </button>
              <button class="drawer-extra-btn" data-mutating @click="d.downloadDanmakuNow">
                <i class="fa-solid fa-comment-dots"></i> 下载弹幕
              </button>
              <button class="drawer-extra-btn" data-mutating @click="d.downloadCommentsNow">
                <i class="fa-solid fa-comments"></i> 下载评论
              </button>
            </div>
          </div>

          <!-- 日志区（按 bvid 过滤，时间倒序）。 -->
          <div class="drawer-section">
            <div class="drawer-section-title">
              日志
              <button class="btn btn-sm btn-ghost" title="刷新日志" :disabled="d.logsLoading.value" @click="d.loadLogs">
                <i class="fa-solid fa-sync-alt" :class="{ 'fa-spin': d.logsLoading.value }"></i> 刷新
              </button>
            </div>
            <div class="drawer-logs">
              <div v-if="d.logsLoading.value" class="drawer-logs-hint"><i class="fa-solid fa-spinner fa-spin"></i> 加载中...</div>
              <div v-else-if="d.logsError.value" class="drawer-logs-hint log-error"><i class="fa-solid fa-triangle-exclamation"></i> {{ d.logsError.value }}</div>
              <div v-else-if="!d.logLines.value.length" class="drawer-logs-hint">暂无该视频的日志</div>
              <div v-for="(l, i) in d.logLines.value" :key="i" class="drawer-log-item" :class="l.cls">
                <span class="drawer-log-time">{{ l.time }}</span>
                <span class="drawer-log-level">{{ l.level }}</span>
                <span class="drawer-log-msg">{{ l.msg }}</span>
              </div>
            </div>
          </div>
        </template>
      </template>
    </div>
  </div>
</template>
