<script setup lang="ts">
/**
 * 设置页「下载」分组 sections（从 TabSettings.vue 拆出）：
 * 弹幕和评论下载、弹幕烧录参数、CC 字幕、下载目录整理、存储、视频保留数、MD5 完整性校验。
 */
import { computed } from 'vue';
import { useSettingsStore } from '@/stores/settings';
import { useSettingsSections } from '@/composables/useSettingsSections';

const settings = useSettingsStore();
const { collapsedSections, inGroup } = useSettingsSections();

/** 评论回复模式说明文字（对齐老框架 onCommentsReplyModeChange：all 时 tone-error + 图标）。 */
const commentsReplyModeNote = computed(() => {
  const mode = settings.settings.comments_reply_mode;
  if (mode === 'all') return '展开每条评论的全部子评论，会显著增加请求量，触发风控的概率更高。追求稳定请用“仅热门回复”。';
  return '仅取接口自带的约 3 条热门回复，无额外请求，最不易触发风控。';
});
const commentsReplyModeWarning = computed(() => settings.settings.comments_reply_mode === 'all');

const pathPreviewText = computed(() => {
  const template = settings.settings.file_naming_template?.trim()
    || (settings.settings.auto_organize ? '{uid}/{title}' : '{title}');
  return `./data/downloads/${template
    .replace(/\{uid\}/g, '123456')
    .replace(/\{up\}/g, '示例UP主')
    .replace(/\{date\}/g, '2026-07-26')
    .replace(/\{title\}/g, '示例视频')
    .replace(/\{bvid\}/g, 'BV1xx411c7mD')
    .replace(/\{quality\}/g, '1080p')
    .replace(/\{codec\}/g, 'av1')
    .replace(/\{page\}/g, '1')
    .replace(/\{type\}/g, 'video')}.mp4`;
});
</script>

<template>
  <!-- 弹幕评论 -->
  <div v-show="inGroup('danmaku')" :class="{ collapsed: collapsedSections.has('danmaku') }" class="section-collapsible" data-section="danmaku">
    <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-comments"></i> 弹幕和评论下载设置</h2></div>
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
          <div class="form-note" id="comments-reply-mode-note" :class="{ 'tone-error': commentsReplyModeWarning }"><i v-if="commentsReplyModeWarning" class="fa-solid fa-exclamation-triangle"></i> {{ commentsReplyModeNote }}</div>
        </div>
        <div class="form-group form-full">
          <label for="setting-comments-filter-regex">评论正则过滤</label>
          <input type="text" id="setting-comments-filter-regex" class="form-control" placeholder="留空表示不过滤，例如: (广告|加群|微信)"
                 :value="settings.settings.comments_filter_regex || ''"
                 @change="settings.update({ comments_filter_regex: ($event.target as HTMLInputElement).value })" />
          <div class="form-note">命中该正则的评论/回复不会写入结果（作用于评论正文，全局生效）。</div>
        </div>
        <div class="form-group">
          <label for="setting-sidecar-archive-mode">弹幕/评论归档</label>
          <select id="setting-sidecar-archive-mode" class="form-control" :value="settings.settings.sidecar_archive_mode || 'overwrite'"
                  @change="settings.update({ sidecar_archive_mode: ($event.target as HTMLSelectElement).value })">
            <option value="overwrite">仅保留最新文件（默认）</option>
            <option value="keep_latest_n">保留最近 N 次归档</option>
            <option value="keep_all">永久保留全部归档</option>
          </select>
          <div class="form-note">固定名文件始终保存最新内容，供查看和烧录使用。</div>
        </div>
        <div v-show="settings.settings.sidecar_archive_mode === 'keep_latest_n'" class="form-group" id="sidecar-archive-limit-group">
          <label for="setting-sidecar-archive-limit">归档保留次数</label>
          <input type="number" id="setting-sidecar-archive-limit" class="form-control" min="1" max="50"
                 :value="settings.settings.sidecar_archive_limit ?? 3"
                 @change="settings.update({ sidecar_archive_limit: Number(($event.target as HTMLInputElement).value) })" />
          <div class="form-note">范围: 1-50。固定名最新文件不计入归档次数。</div>
        </div>
      </div>
    </div>
  </div>

  <!-- 弹幕烧录参数 -->
  <div v-show="inGroup('burn')" :class="{ collapsed: collapsedSections.has('burn') }" class="section-collapsible" data-section="burn">
    <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-closed-captioning"></i> 弹幕烧录参数</h2></div>
    <div class="section-body">
      <div class="settings-grid">
        <div class="form-group">
          <label for="setting-burn-opacity">弹幕透明度</label>
          <input type="number" id="setting-burn-opacity" class="form-control" min="0.1" max="1" step="0.1"
                 :value="settings.settings.burn_opacity ?? 0.6"
                 @change="settings.update({ burn_opacity: Number(($event.target as HTMLInputElement).value) })" />
          <div class="form-note">范围: 0.1~1.0，默认 0.6</div>
        </div>
        <div class="form-group">
          <label for="setting-burn-font-size-scale">字号缩放</label>
          <input type="number" id="setting-burn-font-size-scale" class="form-control" min="0.5" max="2" step="0.1"
                 :value="settings.settings.burn_font_size_scale ?? 1"
                 @change="settings.update({ burn_font_size_scale: Number(($event.target as HTMLInputElement).value) })" />
          <div class="form-note">范围: 0.5~2.0，默认 1.0</div>
        </div>
        <div class="form-group">
          <label for="setting-burn-scroll-time">滚动弹幕时长（秒）</label>
          <input type="number" id="setting-burn-scroll-time" class="form-control" min="1" max="60" step="0.5"
                 :value="settings.settings.burn_scroll_time ?? 8"
                 @change="settings.update({ burn_scroll_time: Number(($event.target as HTMLInputElement).value) })" />
          <div class="form-note">范围: 1~60，默认 8</div>
        </div>
        <div class="form-group">
          <label for="setting-burn-fix-time">固定弹幕时长（秒）</label>
          <input type="number" id="setting-burn-fix-time" class="form-control" min="1" max="60" step="0.5"
                 :value="settings.settings.burn_fix_time ?? 4"
                 @change="settings.update({ burn_fix_time: Number(($event.target as HTMLInputElement).value) })" />
          <div class="form-note">范围: 1~60，默认 4</div>
        </div>
        <div class="form-group">
          <label for="setting-burn-bottom-reserve">底部保留高度（像素）</label>
          <input type="number" id="setting-burn-bottom-reserve" class="form-control" min="0" max="200"
                 :value="settings.settings.burn_bottom_reserve ?? 50"
                 @change="settings.update({ burn_bottom_reserve: Number(($event.target as HTMLInputElement).value) })" />
          <div class="form-note">范围: 0~200，默认 50；避免弹幕遮挡字幕</div>
        </div>
        <div class="form-group">
          <label for="setting-burn-font-family">烧录字体</label>
          <select id="setting-burn-font-family" class="form-control" :value="settings.settings.burn_font_family || 'auto'"
                  @change="settings.update({ burn_font_family: ($event.target as HTMLSelectElement).value })">
            <option value="auto">自动（当前平台默认）</option><option value="Microsoft YaHei UI">Microsoft YaHei UI</option>
            <option value="Noto Sans CJK SC">Noto Sans CJK SC</option><option value="Arial">Arial</option>
          </select>
          <div class="form-note">只影响 ASS 烧录，不修改原始弹幕文件。</div>
        </div>
        <div class="form-group">
          <label for="setting-burn-color-mode">弹幕颜色</label>
          <select id="setting-burn-color-mode" class="form-control" :value="settings.settings.burn_color_mode || 'source'"
                  @change="settings.update({ burn_color_mode: ($event.target as HTMLSelectElement).value })">
            <option value="source">保留 B 站原始颜色</option><option value="uniform">使用统一颜色</option>
          </select>
        </div>
        <div class="form-group">
          <label for="setting-burn-color">统一颜色（HEX）</label>
          <input type="text" id="setting-burn-color" class="form-control" maxlength="6" placeholder="FFFFFF"
                 :value="settings.settings.burn_color || 'FFFFFF'"
                 @change="settings.update({ burn_color: ($event.target as HTMLInputElement).value })" />
          <div class="form-note">仅在“使用统一颜色”时生效，例如 FFFFFF。</div>
        </div>
      </div>
    </div>
  </div>

  <!-- CC 字幕 -->
  <div v-show="inGroup('subtitle')" :class="{ collapsed: collapsedSections.has('subtitle') }" class="section-collapsible" data-section="subtitle">
    <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-closed-captioning"></i> CC 字幕</h2></div>
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
          <div class="form-note">关闭时跳过 lan 以 ai- 开头的字幕</div>
        </div>
        <div class="form-group form-full">
          <label for="setting-subtitle-languages">语言过滤（逗号分隔，留空下载全部）</label>
          <input type="text" id="setting-subtitle-languages" class="form-control"
                 placeholder="zh-CN,zh-Hans"
                 :value="settings.settings.subtitle_languages || ''"
                 @change="settings.update({ subtitle_languages: ($event.target as HTMLInputElement).value })" />
          <div class="form-note">例如 zh-CN,zh-Hans；留空表示下载全部语言</div>
        </div>
      </div>
    </div>
  </div>

  <!-- 下载目录整理（对齐老框架 data-section="path"：自动分文件夹 + 目录模板 + 同名策略 + 路径预览） -->
  <div v-show="inGroup('path')" :class="{ collapsed: collapsedSections.has('path') }" class="section-collapsible" data-section="path">
    <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-folder-open"></i> 下载目录整理</h2></div>
    <div class="section-body">
      <div class="settings-grid">
        <div class="form-group form-full">
          <label for="setting-auto-organize">自动分文件夹</label>
          <div class="switch-group">
            <span class="switch-label">按博主UID自动创建子文件夹</span>
            <label class="toggle-switch">
              <input type="checkbox" id="setting-auto-organize" aria-label="按博主 UID 自动创建子文件夹"
                     :checked="!!settings.settings.auto_organize"
                     @change="settings.update({ auto_organize: ($event.target as HTMLInputElement).checked })" />
              <span class="slider"></span>
            </label>
          </div>
        </div>
        <div class="form-group form-full">
          <label for="setting-path-template">目录模板</label>
          <input id="setting-path-template" class="form-control" placeholder="{uid}/{title}"
                 :value="settings.settings.file_naming_template || ''"
                 @change="settings.update({ file_naming_template: ($event.target as HTMLInputElement).value })" />
          <div class="form-note">可用变量：{up}、{uid}、{date}、{title}、{bvid}、{quality}、{codec}、{page}、{type}</div>
        </div>
        <div class="form-group">
          <label for="setting-conflict-strategy">同名不同内容时</label>
          <select id="setting-conflict-strategy" class="form-control"
                  :value="settings.settings.conflict_strategy || 'suffix'"
                  @change="settings.update({ conflict_strategy: ($event.target as HTMLSelectElement).value as any })">
            <option value="suffix">保留并加时间戳</option>
            <option value="skip">跳过新文件</option>
            <option value="overwrite">覆盖已有文件</option>
          </select>
        </div>
        <div class="form-group form-full">
          <span class="form-label">当前下载路径预览</span>
          <div id="path-preview" class="path-preview-box">
            <i class="fa-solid fa-file-video"></i>
            <span id="path-preview-text">{{ pathPreviewText }}</span>
          </div>
          <div class="form-note">下载路径固定在 data/downloads 文件夹下</div>
        </div>
      </div>
    </div>
  </div>

  <!-- 存储设置。 -->
  <div v-show="inGroup('storage')" :class="{ collapsed: collapsedSections.has('storage') }" class="section-collapsible" data-section="storage">
    <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-hdd"></i> 存储设置</h2></div>
    <div class="section-body">
      <div class="settings-grid">
        <div class="form-group">
          <label for="setting-history-limit">历史记录上限</label>
          <input type="number" id="setting-history-limit" class="form-control" min="100" max="5000"
                 :value="settings.settings.history_limit ?? 1000"
                 @change="settings.update({ history_limit: Number(($event.target as HTMLInputElement).value) })" />
          <div class="form-note">范围: 100-5000，建议 1000；历史清理按此设置生效</div>
        </div>
        <div class="form-group">
          <label for="setting-log-limit">日志上限</label>
          <input type="number" id="setting-log-limit" class="form-control" min="50" max="500"
                 :value="settings.settings.log_limit ?? 100"
                 @change="settings.update({ log_limit: Number(($event.target as HTMLInputElement).value) })" />
          <div class="form-note">范围: 50-500，建议 100</div>
        </div>
      </div>
    </div>
  </div>

  <!-- 视频保留数（全局默认，统一生效）。 -->
  <div v-show="inGroup('retain')" :class="{ collapsed: collapsedSections.has('retain') }" class="section-collapsible" data-section="retain">
    <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-database"></i> 视频保留数</h2></div>
    <div class="section-body">
      <div class="settings-grid">
        <div class="form-group form-full">
          <label for="setting-per-blogger-retain-default">全局默认保留数</label>
          <input type="number" id="setting-per-blogger-retain-default" class="form-control" min="0" max="1000"
                 :value="settings.settings.per_blogger_retain_default ?? 0"
                 @change="settings.update({ per_blogger_retain_default: Number(($event.target as HTMLInputElement).value) })" />
          <div class="form-note">0 = 不限制；≥1 时每个博主只保留最新 N 条视频（超出的按发布时间删除文件 + 记录），对所有博主统一生效。</div>
        </div>
      </div>
    </div>
  </div>

  <!-- MD5 完整性校验。 -->
  <div v-show="inGroup('verify')" :class="{ collapsed: collapsedSections.has('verify') }" class="section-collapsible" data-section="verify">
    <div class="section-header" role="button" tabindex="0"><h2><i class="fa-solid fa-fingerprint"></i> MD5 完整性校验</h2></div>
    <div class="section-body">
      <div class="settings-grid">
        <div class="form-group form-full">
          <label for="setting-verify-mode">校验模式</label>
          <select id="setting-verify-mode" class="form-control"
                  :value="settings.settings.verify_mode || 'off'"
                  @change="settings.update({ verify_mode: ($event.target as HTMLSelectElement).value as any })">
            <option value="off">关闭</option>
            <option value="on_completion">下载完成时校验一次</option>
            <option value="periodic">周期校验（按天数）</option>
          </select>
          <div class="form-note">下载完成时算一次 MD5 存库；周期模式另起后台 worker 定时复核</div>
        </div>
        <div class="form-group" id="verify-periodic-group" v-show="settings.settings.verify_mode === 'periodic'">
          <label for="setting-verify-periodic-days">校验间隔（天）</label>
          <input type="number" id="setting-verify-periodic-days" class="form-control" min="1" max="90"
                 :value="settings.settings.verify_periodic_days ?? 7"
                 @change="settings.update({ verify_periodic_days: Number(($event.target as HTMLInputElement).value) })" />
          <div class="form-note">范围: 1-90，建议 7</div>
        </div>
        <div class="form-group" id="verify-batch-group" v-show="settings.settings.verify_mode === 'periodic'">
          <label for="setting-verify-periodic-batch">单次批量</label>
          <input type="number" id="setting-verify-periodic-batch" class="form-control" min="1" max="200"
                 :value="settings.settings.verify_periodic_batch ?? 20"
                 @change="settings.update({ verify_periodic_batch: Number(($event.target as HTMLInputElement).value) })" />
          <div class="form-note">范围: 1-200，建议 20。每 60 秒扫一批最久未校验的</div>
        </div>
        <div class="form-group">
          <label for="setting-verify-concurrency">校验并发数</label>
          <input type="number" id="setting-verify-concurrency" class="form-control" min="1" max="16"
                 :value="settings.settings.verify_concurrency ?? 4"
                 @change="settings.update({ verify_concurrency: Number(($event.target as HTMLInputElement).value) })" />
          <div class="form-note">范围: 1-16，建议 4；后台 MD5 校验线程数</div>
        </div>
      </div>
    </div>
  </div>
</template>
