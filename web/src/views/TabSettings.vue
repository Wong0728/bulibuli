<script setup lang="ts">
import { ref, computed, onMounted } from 'vue';
import { useSettingsStore } from '@/stores/settings';
import { useAuthStore } from '@/stores/auth';
import { useToastStore } from '@/stores/toast';
import { confirmDialog } from '@/composables/confirm';

const settings = useSettingsStore();
const auth = useAuthStore();
const toast = useToastStore();

// 扫码登录：通过 app store 共享开关（顶部"未登录"按钮也共用）
import { useAppStore } from '@/stores/app';
const app = useAppStore();

type Group = 'basic' | 'downloads' | 'advanced' | 'security';
const group = ref<Group>('basic');

onMounted(async () => {
  await settings.load();
  await settings.loadUpdateStatus();
});

const timePoints = computed({
  get: () => settings.settings.time_points || [],
  set: (v) => settings.update({ time_points: v }),
});

function addTimePoint() {
  const list = [...(settings.settings.time_points || [])];
  list.push(24);
  // 自动去重升序
  const uniq = Array.from(new Set(list)).sort((a, b) => a - b);
  settings.update({ time_points: uniq });
}
function removeTimePoint(hours: number) {
  const list = (settings.settings.time_points || []).filter((x: number) => x !== hours);
  settings.update({ time_points: list });
}

async function saveSettings() {
  try { const ok = await settings.save(); if (ok) toast.success('已保存'); else toast.error('保存失败'); }
  catch (e: any) { toast.error(e?.message || '保存失败'); }
}

async function resetSettings() {
  if (!await confirmDialog({ title: '重置设置', message: '确认恢复默认设置？', tone: 'danger' })) return;
  try { const ok = await settings.reset(); if (ok) toast.success('已重置'); else toast.error('失败'); }
  catch (e: any) { toast.error(e?.message || '失败'); }
}

async function restartAria2() {
  try { await settings.restartAria2(); toast.success('aria2 已重启'); }
  catch (e: any) { toast.error(e?.message || '重启失败'); }
}

// 更新管理
const checkingUpdate = ref(false);
const applyingUpdate = ref(false);
async function onCheckUpdate() {
  checkingUpdate.value = true;
  try {
    await settings.checkUpdate();
    if (settings.updateStatus.has_update) {
      toast.success(`发现新版本 ${settings.updateStatus.latest_version}`);
    } else {
      toast.info('当前已是最新版本');
    }
  } catch (e: any) {
    toast.error(e?.message || '检查更新失败');
  } finally {
    checkingUpdate.value = false;
  }
}
async function onApplyUpdate() {
  if (!await confirmDialog({ title: '立即更新', message: '更新将下载并替换程序文件，确认继续？', tone: 'danger' })) return;
  applyingUpdate.value = true;
  try {
    const r: any = await settings.applyUpdate();
    if (r?.applied) {
      toast.success('更新已应用，请重启程序');
    } else {
      toast.info('已是最新版本');
    }
  } catch (e: any) {
    toast.error(e?.message || '更新失败');
  } finally {
    applyingUpdate.value = false;
  }
}

// 日志查看
const loadingLogs = ref(false);
const logLevel = ref('');
async function onLoadLogs() {
  loadingLogs.value = true;
  try {
    await settings.loadLogs(500, logLevel.value || undefined);
  } finally {
    loadingLogs.value = false;
  }
}

const showManualCookie = ref(false);
const manualCookieText = ref('');
function toggleManualCookie() { showManualCookie.value = !showManualCookie.value; }
async function saveManualCookie() {
  if (!manualCookieText.value.trim()) {
    toast.warn('请输入 Cookie 内容');
    return;
  }
  try {
    await auth.saveCookies(manualCookieText.value);
    toast.success('Cookie 已保存');
    showManualCookie.value = false;
    manualCookieText.value = '';
  } catch (e: any) {
    toast.error(e?.message || '保存失败');
  }
}

function setGroup(g: Group) { group.value = g; }

/** 主题切换：立即把状态写到 store 并在 <html> 上反映，等同名的字段持久化
 *  通过 settings.update → save() 完成（与后端 appearance.theme 对应）。 */
function onThemeChange(v: string) {
  settings.update({ theme: v as any });
  document.documentElement.dataset.theme = v;
}

/** group 过滤：基础/下载/高级/安全 各自展示的 section。 */
const sectionGroups: Record<string, Group[]> = {
  account: ['basic'],
  appearance: ['basic'],
  query: ['downloads', 'basic'],
  parallel: ['downloads'],
  danmaku: ['downloads'],
  smart: ['downloads'],
  aria2: ['downloads', 'advanced'],
  ffmpeg: ['advanced'],
  template: ['advanced'],
  organize: ['advanced'],
  burn: ['advanced'],
  subtitle_cc: ['advanced'],
  verify: ['advanced'],
  board: ['advanced'],
  monitor: ['advanced'],
  refresh: ['advanced'],
  live: ['advanced'],
  storage: ['advanced'],
  update: ['advanced'],
  logs: ['advanced'],
  security: ['security'],
};
function inGroup(section: string) {
  return (sectionGroups[section] || []).includes(group.value);
}
</script>

<style scoped>
.settings-logs-panel {
  max-height: 360px;
  overflow-y: auto;
  border: 1px solid var(--color-border, #e5e5e5);
  border-radius: 6px;
  margin-top: 8px;
  font-family: var(--font-mono, 'JetBrains Mono', monospace);
  font-size: 12px;
  background: var(--color-bg-secondary, #fafafa);
}
.log-row {
  display: grid;
  grid-template-columns: 90px 60px 1fr;
  gap: 8px;
  padding: 4px 10px;
  border-bottom: 1px solid var(--color-border, #eaeaea);
  align-items: center;
}
.log-row:last-child { border-bottom: none; }
.log-time { color: var(--color-text-secondary, #888); }
.log-level {
  font-weight: 600;
  text-align: center;
  border-radius: 3px;
  padding: 0 4px;
}
.log-row.log-info .log-level { color: #1976d2; }
.log-row.log-warn .log-level { color: #b8860b; }
.log-row.log-error .log-level { color: #c0392b; }
.log-msg { word-break: break-all; }
.update-status-row {
  display: flex;
  align-items: center;
  gap: 16px;
  flex-wrap: wrap;
}
.update-badge-new {
  background: #28a745;
  color: #fff;
  padding: 2px 8px;
  border-radius: 10px;
  font-size: 12px;
}
</style>

<template>
  <section class="tab-panel">
    <div class="settings-fragment" aria-label="设置分组">
      <div class="settings-group-switcher" role="toolbar" aria-label="设置分组">
        <button type="button" class="btn btn-sm" :aria-pressed="group === 'basic'" :class="{ 'btn-primary': group === 'basic' }" data-settings-group="basic" aria-controls="settings-sections" @click="setGroup('basic')">基础</button>
        <button type="button" class="btn btn-sm" :aria-pressed="group === 'downloads'" :class="{ 'btn-primary': group === 'downloads' }" data-settings-group="downloads" aria-controls="settings-sections" @click="setGroup('downloads')">下载</button>
        <button type="button" class="btn btn-sm" :aria-pressed="group === 'advanced'" :class="{ 'btn-primary': group === 'advanced' }" data-settings-group="advanced" aria-controls="settings-sections" @click="setGroup('advanced')">高级</button>
        <button type="button" class="btn btn-sm" :aria-pressed="group === 'security'" :class="{ 'btn-primary': group === 'security' }" data-settings-group="security" aria-controls="settings-sections" @click="setGroup('security')">安全</button>
      </div>
      <p class="form-note settings-fragment-note">设置按用途分组；首次进入默认显示基础设置。</p>

      <div class="settings-sections" id="settings-sections">
        <!-- B 站账号登录 -->
        <div v-show="inGroup('account')" class="section-collapsible" data-section="account">
          <div class="section-header">
            <h2><i class="fa-solid fa-user-circle"></i> B站账号登录</h2>
          </div>
          <div class="section-body">
            <div class="form-section">
              <div class="form-group form-full">
                <div id="cookie-login-status" class="login-status-box">
                  <span class="login-user-sub">
                    <i v-if="auth.isAuthenticated" class="fa-solid fa-user-check"></i>
                    <i v-else class="fa-solid fa-user-xmark"></i>
                    {{ auth.isAuthenticated
                      ? `已登录：${auth.state.user?.name || '用户'}`
                      : '未登录' }}
                  </span>
                </div>
                <div class="btn-group account-action-group">
                  <button type="button" class="btn btn-primary" data-action="show-qr-login" @click="app.openCookieLogin()">
                    <i class="fa-solid fa-qrcode"></i> 扫码登录 / 切换账号
                  </button>
                  <button type="button" class="btn btn-danger" data-action="logout-account" @click="auth.logout()">
                    <i class="fa-solid fa-right-from-bracket"></i> 退出登录
                  </button>
                  <button type="button" class="btn btn-ghost" data-action="toggle-manual-cookie" @click="toggleManualCookie">
                    <i class="fa-solid fa-keyboard"></i> 手动粘贴 Cookie
                  </button>
                </div>
                <div class="form-note">推荐扫码登录；登录后 Cookie 由服务器安全保存，页面不再显示明文。切换账号可重新扫码或手动粘贴其他账号的 Cookie。</div>

                <div v-show="showManualCookie" class="manual-cookie-panel" id="manual-cookie-box">
                  <label class="sr-only" for="manual-cookies">B站 Cookie</label>
                  <textarea id="manual-cookies" v-model="manualCookieText" class="form-control" placeholder="粘贴其他账号的 Cookie 字符串（含 SESSDATA），点击「保存并登录」切换账号"></textarea>
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
        <div v-show="inGroup('appearance')" class="section-collapsible" data-section="appearance">
          <div class="section-header"><h2><i class="fa-solid fa-palette"></i> 外观</h2></div>
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
        <div v-show="inGroup('query')" class="section-collapsible" data-section="query">
          <div class="section-header"><h2><i class="fa-solid fa-search"></i> 查询设置</h2></div>
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
                  <option value="126">杜比视界</option>
                  <option value="127">8K</option>
                </select>
                <div class="form-note">需要相应会员权限</div>
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
        <div v-show="inGroup('parallel')" class="section-collapsible" data-section="parallel">
          <div class="section-header"><h2><i class="fa-solid fa-layer-group"></i> 并行下载设置</h2></div>
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

        <!-- 弹幕评论 -->
        <div v-show="inGroup('danmaku')" class="section-collapsible" data-section="danmaku">
          <div class="section-header"><h2><i class="fa-solid fa-comments"></i> 弹幕和评论下载设置</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group form-full">
                <div class="switch-group">
                  <span class="switch-label">自动下载弹幕</span>
                  <label class="toggle-switch">
                    <input type="checkbox" id="setting-auto-download-danmaku" aria-label="自动下载弹幕"
                           :checked="!!settings.settings.auto_download_danmaku"
                           @change="settings.update({ auto_download_danmaku: ($event.target as HTMLInputElement).checked })" />
                    <span class="slider"></span>
                  </label>
                </div>
                <div class="form-note">自动监控时同步下载弹幕</div>
              </div>
              <div class="form-group form-full">
                <div class="switch-group">
                  <span class="switch-label">自动下载评论</span>
                  <label class="toggle-switch">
                    <input type="checkbox" id="setting-auto-download-comments" aria-label="自动下载评论"
                           :checked="!!settings.settings.auto_download_comments"
                           @change="settings.update({ auto_download_comments: ($event.target as HTMLInputElement).checked })" />
                    <span class="slider"></span>
                  </label>
                </div>
                <div class="form-note">自动监控时同步下载评论</div>
              </div>
              <div class="form-group">
                <label for="setting-comments-main-limit">主评论数量</label>
                <input type="number" id="setting-comments-main-limit" class="form-control" min="1" max="100"
                       :value="settings.settings.comments_main_limit ?? 30"
                       @change="settings.update({ comments_main_limit: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围: 1-100，建议 30</div>
              </div>
              <div class="form-group">
                <label for="setting-comments-reply-mode">每条评论的回复</label>
                <select id="setting-comments-reply-mode" class="form-control"
                        :value="settings.settings.comments_reply_mode ?? 'hot3'"
                        @change="settings.update({ comments_reply_mode: ($event.target as HTMLSelectElement).value as any })">
                  <option value="hot3">仅热门回复（约 3 条，最稳）</option>
                  <option value="all">全部回复（展开子评论，可能触发风控）</option>
                </select>
                <div class="form-note" id="comments-reply-mode-note">仅取接口自带的约 3 条热门回复，无额外请求，最不易触发风控。</div>
              </div>
              <div class="form-group form-full">
                <label for="setting-comments-filter-regex">评论正则过滤</label>
                <input type="text" id="setting-comments-filter-regex" class="form-control" placeholder="留空表示不过滤，例如: (广告|加群|微信)"
                       :value="settings.settings.comments_filter_regex || ''"
                       @change="settings.update({ comments_filter_regex: ($event.target as HTMLInputElement).value })" />
                <div class="form-note">命中该正则的评论/回复不会写入结果（作用于评论正文，全局生效）。</div>
              </div>
            </div>
          </div>
        </div>

        <!-- 智能下载 -->
        <div v-show="inGroup('smart')" class="section-collapsible" data-section="smart" id="smart-download-card">
          <div class="section-header"><h2><i class="fa-solid fa-brain"></i> 智能下载</h2></div>
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
              <div id="smart-download-settings" class="form-group">
                <label for="setting-min-publish-hours">最小发布时间（小时）</label>
                <input type="number" id="setting-min-publish-hours" class="form-control" min="0" max="72"
                       :value="settings.settings.min_publish_hours ?? 1"
                       @change="settings.update({ min_publish_hours: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围: 0-72，默认 1 小时。视频发布超过此时间后才下载弹幕/评论</div>
              </div>
              <div id="time-points-settings" class="form-group">
                <label for="time-point-input">分时下载时间点（小时）</label>
                <div id="time-point-chips" class="time-point-chips">
                  <span v-for="h in timePoints" :key="h" class="time-point-chip">
                    {{ h }}h
                    <button type="button" class="time-point-remove" @click="removeTimePoint(h)">×</button>
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

        <!-- Aria2 -->
        <div v-show="inGroup('aria2')" class="section-collapsible" data-section="aria2">
          <div class="section-header"><h2><i class="fa-solid fa-download"></i> Aria2 下载设置</h2></div>
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
                <div class="form-note" id="download-mode-note">自动启动内置 aria2c 并通过 RPC 控制，无需手动配置</div>
              </div>
              <div class="form-group form-full">
                <div class="aria2-settings-status">
                  <span><span class="aria2-dot" :class="(settings as any).aria2Ok || (settings as any).health?.aria2_ok ? 'connected' : 'disconnected'" data-aria2-status :title="(settings as any).aria2Ok || (settings as any).health?.aria2_ok ? 'aria2 已连接' : 'aria2 未连接'"></span> 保存连接设置后会立即应用，无需重启应用。</span>
                  <button type="button" class="btn btn-sm" data-action="restart-aria2" @click="restartAria2">
                    <i class="fa-solid fa-rotate"></i> 重新连接 Aria2
                  </button>
                </div>
              </div>
            </div>

            <div id="rpc-settings-panel" class="rpc-settings-panel" :hidden="settings.settings.aria2_mode !== 'external'">
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
          </div>
        </div>

        <!-- FFmpeg 设置 -->
        <div v-show="inGroup('ffmpeg')" class="section-collapsible" data-section="ffmpeg">
          <div class="section-header"><h2><i class="fa-solid fa-film"></i> FFmpeg 设置</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group form-full">
                <label for="setting-ffmpeg-mode">FFmpeg 模式</label>
                <select id="setting-ffmpeg-mode" class="form-control"
                        :value="settings.settings.ffmpeg_mode || 'auto'"
                        @change="settings.update({ ffmpeg_mode: ($event.target as HTMLSelectElement).value as any })">
                  <option value="auto">自动检测（推荐）</option>
                  <option value="system">系统 PATH</option>
                  <option value="embedded">内置 ffmpeg</option>
                  <option value="custom">自定义路径</option>
                </select>
                <div class="form-note">自动检测：系统 PATH &gt; 内置 ffmpeg.exe &gt; 自定义路径</div>
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
                <span class="form-label">当前检测到的路径</span>
                <div id="ffmpeg-detected-path" class="path-preview-box">
                  <template v-if="settings.ffmpegInfo">
                    <code>{{ settings.ffmpegInfo.path || '未检测到' }}</code>
                    <span v-if="settings.ffmpegInfo.version" class="form-note" style="margin-left: 8px;">
                      v{{ settings.ffmpegInfo.version }}
                    </span>
                    <span v-if="!settings.ffmpegInfo.available" class="form-note" style="margin-left: 8px; color: var(--tone-error, #c0392b);">
                      不可用
                    </span>
                  </template>
                  <template v-else>
                    <i class="fa-solid fa-spinner fa-spin"></i> 检测中...
                  </template>
                </div>
              </div>
            </div>
          </div>
        </div>

        <!-- 路径模板 -->
        <div v-show="inGroup('template')" class="section-collapsible" data-section="template">
          <div class="section-header"><h2><i class="fa-solid fa-folder-tree"></i> 路径模板</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group form-full">
                <label for="setting-file-template">文件命名模板</label>
                <input type="text" id="setting-file-template" class="form-control" placeholder="{title}-{bvid}"
                       :value="settings.settings.file_naming_template || ''"
                       @change="settings.update({ file_naming_template: ($event.target as HTMLInputElement).value })" />
                <div class="form-note">可用变量：{title} {bvid} {date} {uname} {cid}</div>
              </div>
              <div class="form-group form-full">
                <label for="setting-folder-template">目录命名模板</label>
                <input type="text" id="setting-folder-template" class="form-control" placeholder="{uname}/{title}"
                       :value="settings.settings.folder_naming_template || ''"
                       @change="settings.update({ folder_naming_template: ($event.target as HTMLInputElement).value })" />
                <div class="form-note">可用变量：{uname} {date} {year} {month}</div>
              </div>
            </div>
          </div>
        </div>

        <!-- 烧录设置 -->
        <div v-show="inGroup('burn')" class="section-collapsible" data-section="burn">
          <div class="section-header"><h2><i class="fa-solid fa-fire"></i> 弹幕烧录设置</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group">
                <label for="setting-burn-font-size">弹幕字号</label>
                <input type="number" id="setting-burn-font-size" class="form-control" min="12" max="60"
                       :value="settings.settings.burn_font_size ?? 28"
                       @change="settings.update({ burn_font_size: Number(($event.target as HTMLInputElement).value) })" />
              </div>
              <div class="form-group">
                <label for="setting-burn-opacity">弹幕透明度 (1-100)</label>
                <input type="number" id="setting-burn-opacity" class="form-control" min="1" max="100"
                       :value="settings.settings.burn_opacity ?? 90"
                       @change="settings.update({ burn_opacity: Number(($event.target as HTMLInputElement).value) })" />
              </div>
              <div class="form-group form-full">
                <div class="switch-group">
                  <span class="switch-label">底部滚动弹幕</span>
                  <label class="toggle-switch">
                    <input type="checkbox" id="setting-burn-bottom" aria-label="底部滚动弹幕"
                           :checked="!!settings.settings.burn_bottom"
                           @change="settings.update({ burn_bottom: ($event.target as HTMLInputElement).checked })" />
                    <span class="slider"></span>
                  </label>
                </div>
              </div>
              <div class="form-group form-full">
                <div class="switch-group">
                  <span class="switch-label">顶部弹幕</span>
                  <label class="toggle-switch">
                    <input type="checkbox" id="setting-burn-top" aria-label="顶部弹幕"
                           :checked="!!settings.settings.burn_top"
                           @change="settings.update({ burn_top: ($event.target as HTMLInputElement).checked })" />
                    <span class="slider"></span>
                  </label>
                </div>
              </div>
            </div>
          </div>
        </div>

        <!-- CC 字幕 -->
        <div v-show="inGroup('subtitle_cc')" class="section-collapsible" data-section="subtitle-cc">
          <div class="section-header"><h2><i class="fa-solid fa-closed-captioning"></i> CC 字幕</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group form-full">
                <div class="switch-group">
                  <span class="switch-label">自动下载 CC 字幕</span>
                  <label class="toggle-switch">
                    <input type="checkbox" id="setting-subtitle-enabled" aria-label="自动下载 CC 字幕"
                           :checked="!!settings.settings.subtitle_enabled"
                           @change="settings.update({ subtitle_enabled: ($event.target as HTMLInputElement).checked })" />
                    <span class="slider"></span>
                  </label>
                </div>
              </div>
              <div class="form-group form-full">
                <div class="switch-group">
                  <span class="switch-label">接受 AI 自动生成字幕</span>
                  <label class="toggle-switch">
                    <input type="checkbox" id="setting-subtitle-accept-ai" aria-label="接受 AI 自动生成字幕"
                           :checked="!!settings.settings.subtitle_accept_ai"
                           @change="settings.update({ subtitle_accept_ai: ($event.target as HTMLInputElement).checked })" />
                    <span class="slider"></span>
                  </label>
                </div>
              </div>
              <div class="form-group form-full">
                <label for="setting-subtitle-languages">字幕语言</label>
                <input type="text" id="setting-subtitle-languages" class="form-control"
                       placeholder="zh-CN,zh-Hans,en-US"
                       :value="settings.settings.subtitle_languages || ''"
                       @change="settings.update({ subtitle_languages: ($event.target as HTMLInputElement).value })" />
                <div class="form-note">逗号分隔；留空则下载所有可用语言</div>
              </div>
            </div>
          </div>
        </div>

        <!-- MD5 完整性校验 -->
        <div v-show="inGroup('verify')" class="section-collapsible" data-section="verify">
          <div class="section-header"><h2><i class="fa-solid fa-fingerprint"></i> MD5 完整性校验</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group">
                <label for="setting-verify-mode">校验模式</label>
                <select id="setting-verify-mode" class="form-control"
                        :value="settings.settings.verify_mode || 'off'"
                        @change="settings.update({ verify_mode: ($event.target as HTMLSelectElement).value as any })">
                  <option value="off">关闭</option>
                  <option value="manual">手动（手动触发时校验）</option>
                  <option value="periodic">周期自动校验</option>
                </select>
                <div class="form-note">周期模式下后端按周期自动重算并比对 MD5</div>
              </div>
              <div class="form-group">
                <label for="setting-verify-periodic-days">校验周期（天）</label>
                <input type="number" id="setting-verify-periodic-days" class="form-control" min="1" max="90"
                       :value="settings.settings.verify_periodic_days ?? 7"
                       @change="settings.update({ verify_periodic_days: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围 1-90</div>
              </div>
              <div class="form-group">
                <label for="setting-verify-periodic-batch">每批校验条数</label>
                <input type="number" id="setting-verify-periodic-batch" class="form-control" min="1" max="200"
                       :value="settings.settings.verify_periodic_batch ?? 20"
                       @change="settings.update({ verify_periodic_batch: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围 1-200</div>
              </div>
              <div class="form-group">
                <label for="setting-verify-concurrency">并发校验数</label>
                <input type="number" id="setting-verify-concurrency" class="form-control" min="1" max="16"
                       :value="settings.settings.verify_concurrency ?? 4"
                       @change="settings.update({ verify_concurrency: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围 1-16</div>
              </div>
            </div>
          </div>
        </div>

        <!-- 看板显示 -->
        <div v-show="inGroup('board')" class="section-collapsible" data-section="board">
          <div class="section-header"><h2><i class="fa-solid fa-chalkboard"></i> 看板显示</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group">
                <label for="setting-path-display-mode">路径显示模式</label>
                <select id="setting-path-display-mode" class="form-control"
                        :value="settings.settings.path_display_mode || 'hidden'"
                        @change="settings.update({ path_display_mode: ($event.target as HTMLSelectElement).value as any })">
                  <option value="hidden">隐藏（默认）</option>
                  <option value="relative">显示相对路径</option>
                  <option value="absolute">显示绝对路径</option>
                </select>
                <div class="form-note">下载记录中本地路径的展示方式</div>
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
                <div class="form-note">允许把已下载产物通过浏览器保存到本机</div>
              </div>
            </div>
          </div>
        </div>

        <!-- 下载目录整理 -->
        <div v-show="inGroup('organize')" class="section-collapsible" data-section="organize">
          <div class="section-header"><h2><i class="fa-solid fa-folder-open"></i> 下载目录整理</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group form-full">
                <div class="switch-group">
                  <span class="switch-label">按博主 UID 自动创建子文件夹</span>
                  <label class="toggle-switch">
                    <input type="checkbox" id="setting-auto-organize" aria-label="按博主 UID 自动创建子文件夹"
                           :checked="!!settings.settings.auto_organize"
                           @change="settings.update({ auto_organize: ($event.target as HTMLInputElement).checked })" />
                    <span class="slider"></span>
                  </label>
                </div>
              </div>
              <div class="form-group">
                <label for="setting-conflict-strategy">同名文件处理</label>
                <select id="setting-conflict-strategy" class="form-control"
                        :value="settings.settings.conflict_strategy || 'suffix'"
                        @change="settings.update({ conflict_strategy: ($event.target as HTMLSelectElement).value as any })">
                  <option value="suffix">保留并加时间戳</option>
                  <option value="skip">跳过新文件</option>
                  <option value="overwrite">覆盖已有文件</option>
                </select>
                <div class="form-note">下载目标路径已存在时如何处理</div>
              </div>
            </div>
          </div>
        </div>

        <!-- 监控行为 -->
        <div v-show="inGroup('monitor')" class="section-collapsible" data-section="monitor">
          <div class="section-header"><h2><i class="fa-solid fa-radar"></i> 监控行为</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group form-full">
                <div class="switch-group">
                  <span class="switch-label">重投检测</span>
                  <label class="toggle-switch">
                    <input type="checkbox" id="setting-detect-reupload" aria-label="重投检测"
                           :checked="settings.settings.detect_reupload !== false"
                           @change="settings.update({ detect_reupload: ($event.target as HTMLInputElement).checked })" />
                    <span class="slider"></span>
                  </label>
                </div>
                <div class="form-note">开启后会标记疑似重投的视频（同一稿件被删除后再次发布）</div>
              </div>
              <div class="form-group">
                <label for="setting-multi-page-mode">多 P 视频处理</label>
                <select id="setting-multi-page-mode" class="form-control"
                        :value="settings.settings.multi_page_mode || 'first'"
                        @change="settings.update({ multi_page_mode: ($event.target as HTMLSelectElement).value as any })">
                  <option value="first">仅第一 P</option>
                  <option value="all">所有分 P</option>
                </select>
                <div class="form-note">仅监控首 P 还是所有分 P 都下载</div>
              </div>
              <div class="form-group">
                <label for="setting-scan-page-limit">扫描页数</label>
                <input type="number" id="setting-scan-page-limit" class="form-control" min="1" max="20"
                       :value="settings.settings.scan_page_limit ?? 5"
                       @change="settings.update({ scan_page_limit: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">单次扫描获取的视频页数（1-20）</div>
              </div>
            </div>
          </div>
        </div>

        <!-- 数据刷新 -->
        <div v-show="inGroup('refresh')" class="section-collapsible" data-section="refresh">
          <div class="section-header"><h2><i class="fa-solid fa-sync-alt"></i> 数据刷新</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group">
                <label for="setting-l1-interval-minutes">L1 刷新间隔（分钟）</label>
                <input type="number" id="setting-l1-interval-minutes" class="form-control" min="1" max="1440"
                       :value="settings.settings.l1_interval_minutes ?? 5"
                       @change="settings.update({ l1_interval_minutes: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围 1-1440；L1 = 轻量元数据刷新周期</div>
              </div>
            </div>
          </div>
        </div>

        <!-- 直播录制 -->
        <div v-show="inGroup('live')" class="section-collapsible" data-section="live">
          <div class="section-header"><h2><i class="fa-solid fa-tower-broadcast"></i> 直播录制</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group">
                <label for="setting-live-max-concurrent">最大并发录制数</label>
                <input type="number" id="setting-live-max-concurrent" class="form-control" min="1" max="8"
                       :value="settings.settings.live_max_concurrent ?? 2"
                       @change="settings.update({ live_max_concurrent: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围 1-8；同时录制的直播间数量</div>
              </div>
              <div class="form-group">
                <label for="setting-live-min-free-space">最小剩余空间（GiB）</label>
                <input type="number" id="setting-live-min-free-space" class="form-control" min="1" max="1024"
                       :value="settings.settings.live_min_free_space_gib ?? 10"
                       @change="settings.update({ live_min_free_space_gib: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">剩余空间低于此值时停止新录制</div>
              </div>
              <div class="form-group">
                <label for="setting-live-max-duration">单次最大录制时长（小时）</label>
                <input type="number" id="setting-live-max-duration" class="form-control" min="1" max="72"
                       :value="settings.settings.live_max_duration_hours ?? 12"
                       @change="settings.update({ live_max_duration_hours: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">超过此时长自动分段</div>
              </div>
              <div class="form-group form-full">
                <label for="setting-live-file-template">录制文件名模板</label>
                <input type="text" id="setting-live-file-template" class="form-control"
                       placeholder="{room_id}_{title}_{date}"
                       :value="settings.settings.live_file_name_template || ''"
                       @change="settings.update({ live_file_name_template: ($event.target as HTMLInputElement).value })" />
                <div class="form-note">支持占位符：{room_id} {title} {date} {time}</div>
              </div>
            </div>
          </div>
        </div>

        <!-- 存储 / 保留 -->
        <div v-show="inGroup('storage')" class="section-collapsible" data-section="storage">
          <div class="section-header"><h2><i class="fa-solid fa-hdd"></i> 存储设置</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group">
                <label for="setting-history-limit">历史记录上限</label>
                <input type="number" id="setting-history-limit" class="form-control" min="100" max="5000"
                       :value="settings.settings.history_limit ?? 1000"
                       @change="settings.update({ history_limit: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围 100-5000</div>
              </div>
              <div class="form-group">
                <label for="setting-log-limit">日志保留条数</label>
                <input type="number" id="setting-log-limit" class="form-control" min="50" max="500"
                       :value="settings.settings.log_limit ?? 100"
                       @change="settings.update({ log_limit: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">范围 50-500</div>
              </div>
              <div class="form-group">
                <label for="setting-per-blogger-retain">单博主默认保留数</label>
                <input type="number" id="setting-per-blogger-retain" class="form-control" min="0" max="1000"
                       :value="settings.settings.per_blogger_retain_default ?? 0"
                       @change="settings.update({ per_blogger_retain_default: Number(($event.target as HTMLInputElement).value) })" />
                <div class="form-note">0 = 不限制</div>
              </div>
            </div>
          </div>
        </div>

        <!-- 更新管理 -->
        <div v-show="inGroup('update')" class="section-collapsible" data-section="update">
          <div class="section-header"><h2><i class="fa-solid fa-arrows-rotate"></i> 更新管理</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group form-full">
                <div class="update-status-row">
                  <span>当前版本：<code>{{ settings.updateStatus.current_version || '—' }}</code></span>
                  <span v-if="settings.updateStatus.latest_version">最新版本：<code>{{ settings.updateStatus.latest_version }}</code></span>
                  <span v-if="settings.updateStatus.has_update" class="update-badge-new">有新版本</span>
                </div>
                <div class="form-note" v-if="settings.updateStatus.last_checked_at">
                  上次检查：{{ new Date((settings.updateStatus.last_checked_at as number) * 1000).toLocaleString() }}
                </div>
              </div>
              <div class="form-group">
                <label for="setting-update-policy">更新策略</label>
                <select id="setting-update-policy" class="form-control"
                        :value="settings.settings.update_policy || 'manual'"
                        @change="settings.update({ update_policy: ($event.target as HTMLSelectElement).value as any })">
                  <option value="auto">自动下载</option>
                  <option value="manual">仅提示</option>
                  <option value="off">关闭检测</option>
                </select>
                <div class="form-note">"自动下载"会下载安装包但不会自动重启。</div>
              </div>
              <div class="form-group form-full btn-group">
                <button class="btn" :disabled="checkingUpdate" @click="onCheckUpdate">
                  <i class="fa-solid fa-cloud-arrow-down"></i>
                  {{ checkingUpdate ? '检查中…' : '检查更新' }}
                </button>
                <button class="btn btn-primary" :disabled="!settings.updateStatus.has_update || applyingUpdate" @click="onApplyUpdate">
                  <i class="fa-solid fa-download"></i>
                  {{ applyingUpdate ? '更新中…' : '立即更新' }}
                </button>
              </div>
            </div>
          </div>
        </div>

        <!-- 日志查看 -->
        <div v-show="inGroup('logs')" class="section-collapsible" data-section="logs">
          <div class="section-header"><h2><i class="fa-solid fa-file-lines"></i> 日志</h2></div>
          <div class="section-body">
            <div class="form-group form-full">
              <div class="btn-group">
                <select v-model="logLevel" class="form-control" style="max-width: 160px;">
                  <option value="">全部级别</option>
                  <option value="info">info</option>
                  <option value="warn">warn</option>
                  <option value="error">error</option>
                </select>
                <button class="btn" :disabled="loadingLogs" @click="onLoadLogs">
                  <i class="fa-solid fa-rotate"></i>
                  {{ loadingLogs ? '加载中…' : '加载最近日志' }}
                </button>
                <span class="form-note" v-if="settings.logs.length">共 {{ settings.logs.length }} 条</span>
              </div>
              <div class="settings-logs-panel" id="settings-logs-panel">
                <div v-if="settings.logs.length === 0" class="empty-state"><p>暂无日志</p></div>
                <div v-for="(l, i) in settings.logs" v-else :key="i" :class="['log-row', `log-${l.level}`]">
                  <span class="log-time">{{ new Date(l.ts).toLocaleTimeString() }}</span>
                  <span class="log-level">{{ (l.level || 'info').toUpperCase() }}</span>
                  <span class="log-msg">{{ l.message }}</span>
                </div>
              </div>
            </div>
          </div>
        </div>

        <!-- 安全 -->
        <div v-show="inGroup('security')" class="section-collapsible" data-section="security">
          <div class="section-header"><h2><i class="fa-solid fa-shield-halved"></i> 安全</h2></div>
          <div class="section-body">
            <div class="settings-grid">
              <div class="form-group form-full">
                <div class="switch-group">
                  <span class="switch-label">启用 API 鉴权</span>
                  <label class="toggle-switch">
                    <input type="checkbox" id="setting-enable-auth" aria-label="启用 API 鉴权"
                           :checked="!!settings.settings.enable_auth"
                           @change="settings.update({ enable_auth: ($event.target as HTMLInputElement).checked })" />
                    <span class="slider"></span>
                  </label>
                </div>
                <div class="form-note">开启后所有 API 调用需要 token 鉴权；关闭后所有内网访问都允许调用。</div>
              </div>
              <div class="form-group form-full">
                <div class="switch-group">
                  <span class="switch-label">仅本机访问</span>
                  <label class="toggle-switch">
                    <input type="checkbox" id="setting-bind-localhost" aria-label="仅本机访问"
                           :checked="!!settings.settings.bind_localhost"
                           @change="settings.update({ bind_localhost: ($event.target as HTMLInputElement).checked })" />
                    <span class="slider"></span>
                  </label>
                </div>
                <div class="form-note">关闭后监听所有网卡；如需从其他机器访问可关闭。</div>
              </div>
            </div>
          </div>
        </div>
      </div>

      <!-- 底部操作 -->
      <div class="settings-footer" style="display: flex; gap: 8px; justify-content: flex-end; margin-top: 16px;">
        <button class="btn" @click="resetSettings"><i class="fa-solid fa-undo"></i> 恢复默认</button>
        <button class="btn btn-primary" :disabled="!settings.dirty || settings.saving" @click="saveSettings">
          <i class="fa-solid fa-save"></i> {{ settings.saving ? '保存中…' : '保存' }}
        </button>
      </div>
    </div>
  </section>
</template>
