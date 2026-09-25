<script setup lang="ts">
/**
 * 设置页「基础」分组 sections（从 TabSettings.vue 拆出）：
 * B站账号登录（扫码 / 手动粘贴 Cookie / 退出登录）、外观（主题）、查询、并行下载、智能下载。
 * 设置数据读写直接走 settings/auth store；折叠与分组显隐来自 useSettingsSections 单例。
 */
import { ref, computed, nextTick } from 'vue';
import { useSettingsStore } from '@/stores/settings';
import { useAuthStore } from '@/stores/auth';
import { useAppStore } from '@/stores/app';
import { useToastStore } from '@/stores/toast';
import { confirmDialog } from '@/composables/confirm';
import { useSettingsSections } from '@/composables/useSettingsSections';
import { imageUrl, imageError } from '@/utils/image';

const settings = useSettingsStore();
const auth = useAuthStore();
const app = useAppStore();
const toast = useToastStore();
const { group, collapsedSections, inGroup } = useSettingsSections();

const isOwner = computed(() => auth.state.role === 'owner');

// --- 手动粘贴 Cookie（对齐老框架 bootstrap.js toggleManualCookie / saveManualCookie）---
const showManualCookie = ref(false);
const manualCookieText = ref('');
function toggleManualCookie() {
  showManualCookie.value = !showManualCookie.value;
  if (showManualCookie.value) {
    // 老框架：展开时清空输入并聚焦。
    manualCookieText.value = '';
    void nextTick(() => document.getElementById('manual-cookies')?.focus());
  }
}
async function saveManualCookie() {
  const cookies = manualCookieText.value.trim();
  if (!cookies) {
    toast.error('请先粘贴 Cookie 内容');
    return;
  }
  try {
    await auth.saveCookies(cookies);
    manualCookieText.value = '';
    showManualCookie.value = false;
    toast.success('账号已保存，正在刷新登录信息...');
  } catch (e: any) {
    toast.error(e?.message || '保存失败');
  }
}

// --- 退出登录（对齐老框架 bootstrap.js logoutAccount：清 Cookie + 注销会话 + reload）---
async function logoutAccount() {
  if (!await confirmDialog({
    title: '退出登录',
    message: '确定要退出当前 B 站账号登录吗？退出后需重新扫码或粘贴 Cookie。',
    confirmText: '退出',
    tone: 'danger',
  })) return;
  // auth store 已完整处理 toast + reload（成功即整页刷新），调用方 await 后无需再 toast。
  await auth.logoutAccount();
}

/** 主题切换：立即把状态写到 store 并在 <html> 上反映（老框架 applyTheme），
 *  持久化通过 settings.update → save() 完成。 */
function onThemeChange(v: string) {
  // store 的 update() 已统一应用并记忆主题，这里不再各自写 data-theme。
  settings.update({ theme: v as any });
}

const timePoints = computed({
  get: () => settings.settings.time_points || [],
  set: (v) => settings.update({ time_points: v }),
});

function addTimePoint() {
  const input = document.getElementById('time-point-input') as HTMLInputElement | null;
  const raw = input?.value.trim() || '';
  if (!raw) return;
  const hours = Number(raw);
  if (!Number.isInteger(hours) || hours < 0 || hours > 72) {
    toast.error('时间点需在 0-72 小时之间');
    return;
  }
  const list = [...(settings.settings.time_points || [])];
  if (list.includes(hours)) {
    toast.warn('该时间点已存在');
    if (input) input.value = '';
    return;
  }
  list.push(hours);
  // 自动去重升序
  const uniq = Array.from(new Set(list)).sort((a, b) => a - b);
  settings.update({ time_points: uniq });
  if (input) input.value = '';
}
function removeTimePoint(hours: number) {
  const list = (settings.settings.time_points || []).filter((x: number) => x !== hours);
  settings.update({ time_points: list });
}
</script>

<template>
  <!-- B 站账号登录（非 Owner 隐藏，对齐老框架 applySessionRole） -->
  <div v-if="isOwner" v-show="inGroup('account')" :class="{ collapsed: collapsedSections.has('account') }" class="section-collapsible" data-section="account">
    <div class="section-header" role="button" tabindex="0">
      <h2><i class="fa-solid fa-user-circle"></i> B站账号登录</h2>
    </div>
    <div class="section-body">
      <div class="form-section">
        <div class="form-group form-full">
          <div id="cookie-login-status" class="login-status-box">
            <template v-if="auth.isCookieValid">
              <template v-if="auth.biliUser?.face">
                <img :src="imageUrl(auth.biliUser.face)" class="login-user-face" alt="" @error="imageError" />
                <span class="login-user-face login-user-face-ph" hidden><i class="fa-solid fa-user"></i></span>
              </template>
              <span v-else class="login-user-face login-user-face-ph"><i class="fa-solid fa-user"></i></span>
              <div class="login-user-meta">
                <span class="login-user-name"><i class="fa-solid fa-user-check"></i> {{ auth.biliUser?.name || '用户' }}</span>
                <span class="login-user-sub">UID {{ auth.biliUser?.mid || '--' }} · Lv{{ auth.cookieStatus.level || 0 }} · 已登录</span>
              </div>
            </template>
            <span v-else class="login-user-sub">
              <i class="fa-solid fa-user-xmark"></i>
              {{ auth.cookieStatusLoaded ? '尚未登录 B 站账号' : '正在检查登录状态…' }}
            </span>
          </div>
          <div class="btn-group account-action-group">
            <button type="button" class="btn btn-primary" data-action="show-qr-login" @click="app.openCookieLogin()">
              <i class="fa-solid fa-qrcode"></i> 扫码登录 / 切换账号
            </button>
            <button type="button" class="btn btn-danger" data-action="logout-account" @click="logoutAccount">
              <i class="fa-solid fa-right-from-bracket"></i> 退出登录
            </button>
            <button type="button" class="btn btn-ghost" data-action="toggle-manual-cookie" @click="toggleManualCookie">
              <i class="fa-solid fa-keyboard"></i> 手动粘贴 Cookie
            </button>
          </div>
          <div class="form-note">推荐扫码登录；登录后 Cookie 由服务器安全保存，页面不再显示明文。切换账号可重新扫码或手动粘贴其他账号的 Cookie。</div>

          <div v-show="showManualCookie" class="manual-cookie-panel" id="manual-cookie-box">
            <label class="sr-only" for="manual-cookies">B站 Cookie</label>
            <textarea id="manual-cookies" v-model="manualCookieText" class="form-control" placeholder="粘贴其他账号的 Cookie 字符串（含 SESSDATA），点击“保存并登录”切换账号"></textarea>
            <div class="btn-group manual-cookie-actions">
              <button type="button" class="btn btn-primary" data-action="save-manual-cookie" @click="saveManualCookie">
                <i class="fa-solid fa-save"></i> 保存并登录
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  </div>

  <!-- 外观 -->
  <div v-show="inGroup('appearance')" :class="{ collapsed: collapsedSections.has('appearance') }" class="section-collapsible" data-section="appearance">
    <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-palette"></i> 外观</h2></div>
    <div class="section-body">
      <div class="settings-grid">
        <div class="form-group">
          <label for="setting-theme">主题</label>
          <select id="setting-theme" class="form-control" :value="settings.settings.theme || 'system'" @change="onThemeChange(($event.target as HTMLSelectElement).value)">
            <option value="system">跟随系统</option>
            <option value="light">浅色</option>
            <option value="dark">深色</option>
          </select>
        </div>
      </div>
    </div>
  </div>

  <!-- 查询设置 -->
  <div v-show="inGroup('query')" :class="{ collapsed: collapsedSections.has('query') }" class="section-collapsible" data-section="query">
    <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-search"></i> 查询设置</h2></div>
    <div class="section-body">
      <div class="settings-grid">
        <div class="form-group">
          <label for="setting-manual-query-limit">手动查询视频数量</label>
          <input type="number" id="setting-manual-query-limit" class="form-control" min="1" max="50"
                 :value="settings.settings.manual_query_limit ?? 10"
                 @change="settings.update({ manual_query_limit: Number(($event.target as HTMLInputElement).value) })" />
          <div class="form-note">范围: 1-50，建议 10</div>
        </div>
        <div class="form-group">
          <label for="setting-auto-query-limit">自动下载视频数量</label>
          <input type="number" id="setting-auto-query-limit" class="form-control" min="1" max="20"
                 :value="settings.settings.auto_query_limit ?? 3"
                 @change="settings.update({ auto_query_limit: Number(($event.target as HTMLInputElement).value) })" />
          <div class="form-note">范围: 1-20，建议 3</div>
        </div>
        <div class="form-group form-full">
          <div class="switch-group">
            <span class="switch-label">跳过充电专属视频</span>
            <label class="toggle-switch">
              <input type="checkbox" id="setting-skip-charge-videos" aria-label="跳过充电专属视频"
                     :checked="!!settings.settings.skip_charge_videos"
                     @change="settings.update({ skip_charge_videos: ($event.target as HTMLInputElement).checked })" />
              <span class="slider"></span>
            </label>
          </div>
          <div class="form-note">开启后充电视频不占"自动下载视频数量"名额，自动下载最新 N 个非充电视频；充电视频会记录到看板（充电后可手动重试）。关闭后充电视频照常占名额，已充电的会自动下载、无权限的记录为不可下载</div>
        </div>
        <div class="form-group">
          <label for="setting-video-quality">视频质量 (qn)</label>
          <select id="setting-video-quality" class="form-control"
                  :value="settings.settings.video_max_quality ?? 80"
                  @change="settings.update({ video_max_quality: Number(($event.target as HTMLSelectElement).value) })">
            <option value="16">360P 流畅</option>
            <option value="32">480P 清晰</option>
            <option value="64">720P 高清</option>
            <option value="80">1080P 高清</option>
            <option value="112">1080P+ 高码率</option>
            <option value="116">1080P60 帧</option>
            <option value="120">4K</option>
            <option value="125">HDR</option>
            <option value="126">杜比视界（仅在线观看可用，下载暂不支持）</option>
            <option value="127">8K（仅在线观看可用，下载暂不支持）</option>
          </select>
          <div class="form-note">需要相应会员权限；126/127 下载链路暂不支持（后端下载白名单最高 125），选择后将按可用画质降级</div>
        </div>
        <div class="form-group">
          <label for="setting-video-format">视频格式 (fnval)</label>
          <select id="setting-video-format" class="form-control"
                  :value="settings.settings.video_format ?? 16"
                  @change="settings.update({ video_format: Number(($event.target as HTMLSelectElement).value) })">
            <option value="0">FLV</option>
            <option value="16">DASH MP4</option>
            <option value="4048">全格式支持</option>
          </select>
          <div class="form-note">建议保持默认</div>
        </div>
        <div class="form-group">
          <label for="setting-min-video-quality">最低允许画质</label>
          <select id="setting-min-video-quality" class="form-control"
                  :value="settings.settings.video_min_quality ?? 16"
                  @change="settings.update({ video_min_quality: Number(($event.target as HTMLSelectElement).value) })">
            <option value="16">360P</option>
            <option value="64">720P</option>
            <option value="80">1080P</option>
          </select>
          <div class="form-note">自动降级不会低于此值</div>
        </div>
        <div class="form-group">
          <label for="setting-codec-preference">编码优先级</label>
          <select id="setting-codec-preference" class="form-control"
                  :value="settings.settings.codec_preference || 'av1,hevc,avc'"
                  @change="settings.update({ codec_preference: ($event.target as HTMLSelectElement).value })">
            <option value="av1,hevc,avc">AV1 → HEVC → AVC</option>
            <option value="hevc,av1,avc">HEVC → AV1 → AVC</option>
            <option value="avc,hevc,av1">AVC → HEVC → AVC</option>
          </select>
        </div>
        <div class="form-group form-full">
          <div class="switch-group">
            <span class="switch-label">允许画质自动降级</span>
            <label class="toggle-switch">
              <input type="checkbox" id="setting-allow-quality-fallback" aria-label="允许画质自动降级"
                     :checked="!!settings.settings.allow_quality_fallback"
                     @change="settings.update({ allow_quality_fallback: ($event.target as HTMLInputElement).checked })" />
              <span class="slider"></span>
            </label>
          </div>
        </div>
      </div>
    </div>
  </div>

  <!-- 并行下载设置 -->
  <div v-show="inGroup('parallel')" :class="{ collapsed: collapsedSections.has('parallel') }" class="section-collapsible" data-section="parallel">
    <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-layer-group"></i> 并行下载设置</h2></div>
    <div class="section-body">
      <div class="settings-grid">
        <div class="form-group">
          <label for="setting-max-parallel">最大并行下载数</label>
          <input type="number" id="setting-max-parallel" class="form-control" min="1" max="10"
                 :value="settings.settings.max_parallel_downloads ?? 3"
                 @change="settings.update({ max_parallel_downloads: Number(($event.target as HTMLInputElement).value) })" />
          <div class="form-note">范围 1-10。应用层：同时处理的下载任务数（几个视频一起下）</div>
        </div>
        <div class="form-group">
          <label for="setting-wait-slot-timeout">等待槽位超时 (秒)</label>
          <input type="number" id="setting-wait-slot-timeout" class="form-control" min="60" max="3600"
                 :value="settings.settings.wait_slot_timeout ?? 300"
                 @change="settings.update({ wait_slot_timeout: Number(($event.target as HTMLInputElement).value) })" />
          <div class="form-note">范围: 60-3600，建议 300</div>
        </div>
      </div>
    </div>
  </div>

  <!-- 智能下载 -->
  <div v-show="inGroup('smart')" :class="{ collapsed: collapsedSections.has('smart') }" class="section-collapsible" data-section="smart" id="smart-download-card">
    <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-brain"></i> 智能下载</h2></div>
    <div class="section-body">
      <div class="settings-grid">
        <div class="form-group form-full">
          <div class="switch-group">
            <span class="switch-label">启用智能下载</span>
            <label class="toggle-switch">
              <input type="checkbox" id="setting-enable-smart-download" aria-label="启用智能下载"
                     :checked="!!settings.settings.enable_smart_download"
                     @change="settings.update({ enable_smart_download: ($event.target as HTMLInputElement).checked })" />
              <span class="slider"></span>
            </label>
          </div>
          <div class="form-note">根据视频发布时间智能决定何时下载弹幕和评论，避免新视频弹幕/评论过少的问题</div>
        </div>
        <div id="smart-download-settings" :class="['form-group', { 'control-disabled': !settings.settings.enable_smart_download }]">
          <label for="setting-min-publish-hours">最小发布时间（小时）</label>
          <input type="number" id="setting-min-publish-hours" class="form-control" min="0" max="72"
                 :value="settings.settings.min_publish_hours ?? 1"
                 @change="settings.update({ min_publish_hours: Number(($event.target as HTMLInputElement).value) })" />
          <div class="form-note">范围: 0-72，默认 1 小时。视频发布超过此时间后才下载弹幕/评论</div>
        </div>
        <div id="time-points-settings" :class="['form-group', { 'control-disabled': !settings.settings.enable_smart_download }]">
          <label for="time-point-input">分时下载时间点（小时）</label>
          <div id="time-point-chips" class="time-point-chips">
            <span v-if="!timePoints.length" class="time-point-empty">未添加时间点</span>
            <span v-for="h in timePoints" :key="h" class="time-point-chip">
              {{ h }}h
              <button type="button" class="time-point-chip-del" title="移除" data-action="remove-time-point" :data-hours="h" @click="removeTimePoint(h)"><i class="fa-solid fa-times"></i></button>
            </span>
          </div>
          <div class="time-point-add">
            <input type="number" id="time-point-input" class="form-control" min="0" max="72" placeholder="小时，如 5" />
            <button type="button" class="btn btn-sm btn-primary" data-action="add-time-point" @click="addTimePoint">
              <i class="fa-solid fa-plus"></i> 添加
            </button>
          </div>
          <div class="form-note">在视频发布后这些小时点分别下载弹幕/评论。范围 0-72，自动去重升序。</div>
        </div>
      </div>
    </div>
  </div>
</template>
