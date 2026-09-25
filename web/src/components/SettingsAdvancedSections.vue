<script setup lang="ts">
/**
 * 设置页「高级」分组 sections（从 TabSettings.vue 拆出）：
 * 看板显示、监控行为、数据刷新、直播录制、更新管理（检查 / 立即更新走 useSettingsUpdate 单例）。
 */
import { useSettingsStore } from '@/stores/settings';
import { useSettingsSections } from '@/composables/useSettingsSections';
import { useSettingsUpdate } from '@/composables/useSettingsUpdate';

const settings = useSettingsStore();
const { collapsedSections, inGroup } = useSettingsSections();
const { applyingUpdate, updateStatusLoaded, updateCheckResult, onCheckUpdate, onApplyUpdate } = useSettingsUpdate();
</script>

<template>
  <!-- 看板显示 -->
  <div v-show="inGroup('board')" :class="{ collapsed: collapsedSections.has('board') }" class="section-collapsible" data-section="board">
    <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-chalkboard"></i> 看板显示</h2></div>
    <div class="section-body">
      <div class="settings-grid">
        <div class="form-group">
          <label for="setting-path-display-mode">路径显示方式</label>
          <select id="setting-path-display-mode" class="form-control"
                  :value="settings.settings.path_display_mode || 'hidden'"
                  @change="settings.update({ path_display_mode: ($event.target as HTMLSelectElement).value as any })">
            <option value="hidden">隐藏路径</option>
            <option value="relative">复制/显示相对路径</option>
            <option value="absolute">复制/显示绝对路径（本机访问时生效）</option>
          </select>
          <div class="form-note">绝对路径与“打开所在目录”仅对与服务同机的客户端（本机浏览器）生效；局域网远程设备仍显示相对路径，避免泄露服务端文件系统信息。</div>
        </div>
        <div class="form-group form-full">
          <div class="switch-group">
            <span class="switch-label">抽屉浏览器下载</span>
            <label class="toggle-switch">
              <input type="checkbox" id="setting-browser-download-enabled" aria-label="抽屉浏览器下载"
                     :checked="settings.settings.browser_download_enabled !== false"
                     @change="settings.update({ browser_download_enabled: ($event.target as HTMLInputElement).checked })" />
              <span class="slider"></span>
            </label>
          </div>
          <div class="form-note">开启后，在下载管理页点击视频打开的抽屉里，可勾选服务器上已下载的视频、音频、弹幕、评论等产物并保存到本机（服务器到本机）；关闭后隐藏入口且接口拒绝访问。</div>
        </div>
      </div>
    </div>
  </div>

  <!-- 监控行为 -->
  <div v-show="inGroup('monitor')" :class="{ collapsed: collapsedSections.has('monitor') }" class="section-collapsible" data-section="monitor">
    <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-satellite-dish"></i> 监控行为</h2></div>
    <div class="section-body">
      <div class="settings-grid">
        <div class="form-group form-full">
          <div class="switch-group">
            <span class="switch-label">重投检测（纯提示）</span>
            <label class="toggle-switch">
              <input type="checkbox" id="setting-detect-reupload" aria-label="重投检测"
                     :checked="settings.settings.detect_reupload !== false"
                     @change="settings.update({ detect_reupload: ($event.target as HTMLInputElement).checked })" />
              <span class="slider"></span>
            </label>
          </div>
          <div class="form-note">开启后，发现标题与近 90 天历史高度相似的新视频时，卡片左上角显示红点，抽屉显示“可能是 BVxxx 的重传”。不会自动重下、不会删旧文件</div>
        </div>
        <div class="form-group form-full">
          <label class="form-label" for="setting-multi-page-mode">多P自动下载范围</label>
          <select id="setting-multi-page-mode" class="form-control"
                  :value="settings.settings.multi_page_mode || 'first'"
                  @change="settings.update({ multi_page_mode: ($event.target as HTMLSelectElement).value as any })">
            <option value="first">仅 P1（保持现状）</option>
            <option value="all">全部分P</option>
          </select>
          <div class="form-note">自动监控发现的多P投稿：选择“仅 P1”只下第一P；选择“全部分P”则每个分P独立入队下载。手动下载不受此项影响，可在抽屉内自选分P</div>
        </div>
        <div class="form-group form-full">
          <label class="form-label" for="setting-scan-page-limit">监控分页上限</label>
          <input type="number" id="setting-scan-page-limit" class="form-control" min="1" max="20"
                 :value="settings.settings.scan_page_limit ?? 5"
                 @change="settings.update({ scan_page_limit: Number(($event.target as HTMLInputElement).value) })" />
          <div class="form-note">范围 1-20，建议 5。监控时每个博主最多拉取最新 N 页视频（每页 30 个）；上限越高，能发现越久远的视频，但首轮扫描更慢</div>
        </div>
      </div>
    </div>
  </div>

  <!-- 数据刷新 -->
  <div v-show="inGroup('refresh')" :class="{ collapsed: collapsedSections.has('refresh') }" class="section-collapsible" data-section="refresh">
    <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-sync-alt"></i> 数据刷新</h2></div>
    <div class="section-body">
      <div class="settings-grid">
        <div class="form-group">
          <label for="setting-l1-interval-minutes">L1 视频数据刷新间隔（分钟）</label>
          <input type="number" id="setting-l1-interval-minutes" class="form-control" min="1" max="1440"
                 :value="settings.settings.l1_interval_minutes ?? 5"
                 @change="settings.update({ l1_interval_minutes: Number(($event.target as HTMLInputElement).value) })" />
          <div class="form-note">范围: 1-1440，建议 5。后端每 N 分钟抽 50 条最久未刷的视频更新播放量/状态</div>
        </div>
      </div>
    </div>
  </div>

  <!-- 直播录制 -->
  <div v-show="inGroup('live-recording')" :class="{ collapsed: collapsedSections.has('live-recording') }" class="section-collapsible" data-section="live-recording">
    <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-tower-broadcast"></i> 直播录制</h2></div>
    <div class="section-body">
      <div class="settings-grid">
        <div class="form-group">
          <label class="form-label" for="setting-live-max-concurrent">并发录制上限（路）</label>
          <input type="number" id="setting-live-max-concurrent" class="form-control" min="1" max="8"
                 :value="settings.settings.live_max_concurrent ?? 2"
                 @change="settings.update({ live_max_concurrent: Number(($event.target as HTMLInputElement).value) })" />
          <div class="form-note">范围 1-8。多房间同时开播时，超出上限的房间不会自动开录</div>
        </div>
        <div class="form-group">
          <label class="form-label" for="setting-live-min-free-space">磁盘余量安全阈值（GiB）</label>
          <input type="number" id="setting-live-min-free-space" class="form-control" min="1" max="1024"
                 :value="settings.settings.live_min_free_space_gib ?? 10"
                 @change="settings.update({ live_min_free_space_gib: Number(($event.target as HTMLInputElement).value) })" />
          <div class="form-note">启动和录制过程中可用空间低于该值时安全停录，避免磁盘写满</div>
        </div>
        <div class="form-group">
          <label class="form-label" for="setting-live-max-duration">单场录制时长上限（小时）</label>
          <input type="number" id="setting-live-max-duration" class="form-control" min="1" max="72"
                 :value="settings.settings.live_max_duration_hours ?? 12"
                 @change="settings.update({ live_max_duration_hours: Number(($event.target as HTMLInputElement).value) })" />
          <div class="form-note">范围 1-72。防止忘关的异常直播永久占用磁盘</div>
        </div>
        <div class="form-group form-full">
          <label for="setting-live-file-template">录制文件名模板</label>
          <input type="text" id="setting-live-file-template" class="form-control"
                 placeholder="{room_id}_{title}_{date}"
                 :value="settings.settings.live_file_name_template || ''"
                 @change="settings.update({ live_file_name_template: ($event.target as HTMLInputElement).value })" />
          <div class="form-note">可用占位符：{room_id} 房间号、{title} 直播标题、{date} 日期、{time} 时间；系统会自动追加时间戳与短后缀避免重名</div>
        </div>
      </div>
    </div>
  </div>

  <!-- 更新（对齐老框架 settings.html update section）。 -->
  <div v-show="inGroup('update')" :class="{ collapsed: collapsedSections.has('update') }" class="section-collapsible" data-section="update">
    <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-cloud-arrow-down"></i> 更新</h2></div>
    <div class="section-body">
      <div class="settings-grid">
        <!-- loadUpdateStatus 失败静默（老框架），版本框保留“正在获取...”。 -->
        <div class="form-group form-full">
          <span class="form-label">当前版本</span>
          <div class="path-preview-box">
            <code id="update-current-version">{{ updateStatusLoaded ? (settings.updateStatus.current_version || '未知') : '正在获取...' }}</code>
          </div>
        </div>
        <div class="form-group form-full">
          <span class="form-label">最新版本</span>
          <div class="path-preview-box">
            <code id="update-latest-version">{{ settings.updateStatus.has_update ? `${settings.updateStatus.latest_version}（有新版本）` : (settings.updateStatus.latest_version || '尚未检查') }}</code>
          </div>
        </div>
        <div class="form-group form-full">
          <label for="setting-update-policy">更新策略</label>
          <select id="setting-update-policy" class="form-control"
                  :value="settings.settings.update_policy || 'manual'"
                  @change="settings.update({ update_policy: ($event.target as HTMLSelectElement).value as any })">
            <option value="manual">仅提示，不自动更新（推荐）</option>
            <option value="auto">检测到新版本自动下载暂存</option>
            <option value="off">关闭检测</option>
          </select>
          <div class="form-note">自动更新只替换程序文件、不触碰 data/ 目录；更新完成后需重启程序生效。</div>
        </div>
        <div class="form-group form-full">
          <div class="btn-group">
            <button type="button" class="btn" data-action="check-update" @click="onCheckUpdate">
              <i class="fa-solid fa-magnifying-glass"></i> 立即检查更新
            </button>
            <button type="button" class="btn btn-primary" data-action="apply-update" :hidden="!settings.updateStatus.has_update" :disabled="applyingUpdate" @click="onApplyUpdate">
              <i class="fa-solid fa-download"></i> 立即更新
            </button>
            <span id="update-check-result" class="test-result-hint">
              <span v-if="updateCheckResult" :class="updateCheckResult.tone">
                <i class="fa-solid" :class="[updateCheckResult.icon, { 'fa-spin': updateCheckResult.spin }]"></i>
                {{ updateCheckResult.text }}
              </span>
            </span>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>
