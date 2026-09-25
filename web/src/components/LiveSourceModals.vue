<script setup lang="ts">
/**
 * 直播源「添加」与「设置」弹窗（从 TabLive.vue 拆出）。
 * - 添加弹窗：解析规则逐字对齐 + 串行添加 + 成功后选中（对齐老框架 live-add-modal）。
 * - 设置弹窗：跨天排期校验 + 空排期兜底 + 实时校验提示（P2-8）。
 * 显隐由父组件 v-model 控制；添加成功后 emit('added') 让父组件选中该房间。
 */
import { ref, computed, watch } from 'vue';
import { useLiveStore, parseRoomTokens, validateScheduleStrict, LIVE_WEEKDAYS } from '@/stores/live';
import { useToastStore } from '@/stores/toast';
import { confirmDialog } from '@/composables/confirm';
import { useModalFocus } from '@/composables/modalFocus';

const props = defineProps<{
  showAdd: boolean;
  showSettings: boolean;
}>();

const emit = defineEmits<{
  (e: 'update:showAdd', value: boolean): void;
  (e: 'update:showSettings', value: boolean): void;
  (e: 'added', roomId: number): void;
}>();

const live = useLiveStore();
const toast = useToastStore();
const weekdays = LIVE_WEEKDAYS;

/** toast 文案：优先取后端 result.message，无（或 'success'）时用老框架兜底文案。 */
function messageOr(message: string | undefined, fallback: string): string {
  return message && message !== 'success' ? message : fallback;
}

// ===== 添加直播源弹窗（P1-4）=====

const addRoomInput = ref('');
const adding = ref(false);
const addModalRoot = ref<HTMLElement | null>(null);

function closeAddModal() { emit('update:showAdd', false); }

async function confirmAdd() {
  if (adding.value) return;
  const roomIds = parseRoomTokens(addRoomInput.value || '');
  if (!roomIds.length) {
    toast.warn('请输入有效的房间号或 live.bilibili.com 链接');
    return;
  }
  adding.value = true;
  const results = { ok: [] as number[], fail: [] as string[] };
  try {
    // 对齐老框架：串行逐个添加，失败记录「房间号：原因」。
    for (const roomId of roomIds) {
      try {
        await live.addSource(roomId);
        results.ok.push(roomId);
      } catch (error: any) {
        results.fail.push(`${roomId}：${error?.message || '未知错误'}`);
      }
    }
    if (results.ok.length) {
      toast.success(`成功添加 ${results.ok.length} 个直播源（自动录制默认关闭）`);
      closeAddModal();
      await live.refreshDashboard();
      const firstOkRoom = results.ok[0];
      if (firstOkRoom != null) emit('added', firstOkRoom);
    }
    if (results.fail.length) {
      toast.error(`添加失败 ${results.fail.length} 个：${results.fail.join('；')}`);
    }
  } finally {
    adding.value = false;
  }
}

// 打开时清空输入（对齐老框架 openAddModal）。
watch(() => props.showAdd, (open) => { if (open) addRoomInput.value = ''; });

// ===== 直播源设置弹窗（P2-8）=====

const settingsAutoRecord = ref(false);
const settingsQuality = ref(10000);
const settingsCaptureMode = ref<'off' | 'standard' | 'full'>('standard');
const settingsScheduleMode = ref<'all-day' | 'weekly'>('all-day');
const settingsSchedule = ref<Record<string, string[][]>>({});
const savingSettings = ref(false);
const settingsModalRoot = ref<HTMLElement | null>(null);
const scheduleError = ref('');

function emptySchedule(): Record<string, string[][]> {
  return Object.fromEntries(weekdays.map(([key]) => [key, [['', ''], ['', '']]]));
}

function clearScheduleEditor() {
  for (const [day] of weekdays) settingsSchedule.value[day] = [['', ''], ['', '']];
}

function readSettingsSchedule(): Record<string, string[]> {
  return Object.fromEntries(Object.entries(settingsSchedule.value).map(([day, windows]) => [
    day,
    windows.map(([start, end]) => start && end ? `${start}-${end}` : '').filter(Boolean),
  ]));
}

function updateScheduleValidation() {
  if (settingsScheduleMode.value === 'all-day') { scheduleError.value = ''; return; }
  scheduleError.value = validateScheduleStrict(readSettingsSchedule());
}

// 输入实时校验（对齐老框架 updateScheduleValidation），切到全天时清空编辑器。
watch([settingsSchedule, settingsScheduleMode], () => updateScheduleValidation(), { deep: true });
watch(settingsScheduleMode, mode => {
  if (mode === 'all-day') clearScheduleEditor();
});

function closeSettingsModal() { emit('update:showSettings', false); }

// 打开时从选中源回填表单（对齐老框架 openSettingsModal）。
watch(() => props.showSettings, (open) => {
  if (!open) return;
  const source = live.selectedSource;
  if (!source) return;
  const schedule = emptySchedule();
  for (const [day, windows] of Object.entries(source.weekly_schedule || {})) {
    const dayWindows = schedule[day];
    if (!dayWindows) continue;
    windows.slice(0, 2).forEach((window, index) => {
      const [start, end] = String(window).split('-');
      dayWindows[index] = [start || '', end || ''];
    });
  }
  settingsAutoRecord.value = !!source.auto_record;
  settingsQuality.value = source.quality || 10000;
  settingsCaptureMode.value = (source.capture_mode || 'standard') as 'off' | 'standard' | 'full';
  settingsScheduleMode.value = source.schedule_all_day === false ? 'weekly' : 'all-day';
  settingsSchedule.value = schedule;
  updateScheduleValidation();
});

async function saveSettingsModal() {
  if (savingSettings.value) return;
  const source = live.selectedSource;
  if (!source) return;
  const schedule = readSettingsSchedule();
  if (settingsScheduleMode.value !== 'all-day') {
    const validationError = validateScheduleStrict(schedule);
    if (validationError) { toast.warn(validationError); return; }
    // 对齐老框架：自动录制开启但排期为空时兜底确认「改为全天」。
    if (settingsAutoRecord.value && !Object.values(schedule).some(windows => windows.length)) {
      const confirmed = await confirmDialog({
        title: '排期为空',
        message: '自动录制已开启，但排期为空，这样永远不会自动开始。要改为"全天允许"吗？',
        confirmText: '改为全天',
        tone: 'default',
      });
      if (!confirmed) return;
      settingsScheduleMode.value = 'all-day';
      clearScheduleEditor();
    }
  }
  const finalAllDay = settingsScheduleMode.value === 'all-day';
  savingSettings.value = true;
  try {
    const r = await live.updateSource(source.id, {
      auto_record: settingsAutoRecord.value,
      quality: settingsQuality.value,
      capture_mode: settingsCaptureMode.value,
      clear_schedule: finalAllDay,
      weekly_schedule: finalAllDay ? null : schedule,
    } as any);
    toast.success(messageOr(r?.message, '直播源设置已保存'));
    closeSettingsModal();
  } catch (e: any) {
    toast.error(`保存失败：${e?.message || '未知错误'}`);
  } finally {
    savingSettings.value = false;
  }
}

const showAddComputed = computed(() => props.showAdd);
const showSettingsComputed = computed(() => props.showSettings);
useModalFocus(showAddComputed, addModalRoot, closeAddModal);
useModalFocus(showSettingsComputed, settingsModalRoot, closeSettingsModal);
</script>

<template>
  <!-- 添加直播源弹窗：与原版 live-add-modal 1:1 同构 -->
  <div v-if="showAdd" ref="addModalRoot" id="live-add-modal" class="modal-overlay" role="dialog" aria-modal="true" aria-labelledby="live-add-title" @click.self="closeAddModal">
    <div class="modal-container">
      <div class="modal-header">
        <i class="fa-solid fa-satellite-dish"></i>
        <span id="live-add-title">添加直播源</span>
        <button type="button" class="modal-close-btn" id="live-add-close-btn" aria-label="关闭" @click="closeAddModal">
          <i class="fa-solid fa-times"></i>
        </button>
      </div>
      <div class="form-section">
        <div class="form-group form-full">
          <label for="live-add-input"><i class="fa-solid fa-link"></i> 房间号或直播链接（支持批量粘贴）</label>
          <textarea id="live-add-input" v-model="addRoomInput" class="form-control live-add-textarea" rows="3"
                    placeholder="例如 123456 或 https://live.bilibili.com/123456，多个房间用换行 / 空格 / 逗号分隔"
                    @keydown.enter.exact.prevent="confirmAdd"></textarea>
          <div class="form-note">添加后默认关闭自动录制；请在房间详情的"设置"中开启自动录制并配置周排期。短号会自动解析为长号。</div>
        </div>
      </div>
      <div class="modal-footer">
        <button class="btn" id="live-add-cancel-btn" @click="closeAddModal">
          <i class="fa-solid fa-times"></i> 取消
        </button>
        <button class="btn btn-primary" id="live-add-confirm-btn" data-network-required="true" data-mutating :disabled="adding" @click="confirmAdd">
          <i :class="adding ? 'fa-solid fa-spinner fa-spin' : 'fa-solid fa-check'"></i> {{ adding ? '添加中…' : '查询并添加' }}
        </button>
      </div>
    </div>
  </div>

  <!-- 直播源设置弹窗：与旧版保持独立弹窗和周排期编辑契约。 -->
  <div v-if="showSettings && live.selectedSource" ref="settingsModalRoot" id="live-source-modal" class="modal-overlay" role="dialog" aria-modal="true" aria-labelledby="live-settings-title" @click.self="closeSettingsModal">
    <div class="modal-container modal-container-wide">
      <div class="modal-header">
        <i class="fa-solid fa-sliders"></i>
        <span id="live-settings-title">直播源设置 · 房间 <span id="live-settings-room-id">{{ live.selectedSource.room_id }}</span></span>
        <button type="button" class="modal-close-btn" id="live-settings-close-btn" aria-label="关闭" @click="closeSettingsModal"><i class="fa-solid fa-times"></i></button>
      </div>
      <div class="form-section">
        <input type="hidden" id="live-source-room-id" :value="live.selectedSource.room_id" />
        <div class="form-group">
          <label class="choice-row" for="live-source-auto">
            <span>自动录制（检测到开播且在排期窗口内时自动开始）</span>
            <span class="toggle-switch"><input id="live-source-auto" v-model="settingsAutoRecord" type="checkbox" /><span class="slider"></span></span>
          </label>
        </div>
        <div class="form-group">
          <label for="live-source-mode"><i class="fa-solid fa-comments"></i> 互动采集</label>
          <select id="live-source-mode" v-model="settingsCaptureMode" class="form-control">
            <option value="standard">标准（推荐，弹幕/礼物/SC/上舰）</option>
            <option value="full">完整原始数据（含全部未知命令）</option>
            <option value="off">关闭（仅录制视频）</option>
          </select>
        </div>
        <div class="form-group">
          <label for="live-source-quality"><i class="fa-solid fa-film"></i> 清晰度上限</label>
          <select id="live-source-quality" v-model.number="settingsQuality" class="form-control">
            <option :value="10000">原画（10000，推荐）</option>
            <option :value="400">蓝光（400）</option>
            <option :value="250">超清（250）</option>
            <option :value="150">高清（150）</option>
            <option :value="80">流畅（80）</option>
          </select>
          <div class="form-note">实际清晰度受账号权限限制可能被 B 站降级，录制卡片会显示实际拿到的清晰度。</div>
        </div>
        <div class="form-group form-full live-schedule-mode-group">
          <label><i class="fa-solid fa-calendar-week"></i> 自动录制时间策略</label>
          <div class="live-schedule-mode" role="radiogroup" aria-label="自动录制时间策略">
            <label class="live-schedule-mode-option">
              <input id="live-source-schedule-all-day" v-model="settingsScheduleMode" type="radio" value="all-day" />
              <span><strong>全天允许</strong><small>开播后随时自动开始</small></span>
            </label>
            <label class="live-schedule-mode-option">
              <input id="live-source-schedule-weekly" v-model="settingsScheduleMode" type="radio" value="weekly" />
              <span><strong>按周排期</strong><small>只在下方时间窗口内自动开始</small></span>
            </label>
          </div>
        </div>
        <div class="form-group form-full">
          <label><i class="fa-solid fa-calendar-week"></i> 自动录制周排期 <span class="form-label-muted">每天最多 2 个时段，支持跨天如 22:00-02:00</span></label>
          <div id="live-tz-note" class="live-tz-note">
            排期按服务器时区生效{{ live.serverTimezone ? `：${live.serverTimezone}` : '' }}{{ live.serverNow ? ` · 服务器当前时间 ${new Date(live.serverNow).toLocaleTimeString('zh-CN', { hour12: false })}` : '' }}
          </div>
          <div id="live-weekly-schedule" class="live-schedule-grid">
            <div class="live-schedule-header" aria-hidden="true"><span>星期</span><span>时段 1</span><span>时段 2</span></div>
            <div v-for="([day, label]) in weekdays" :key="day" class="live-schedule-day">
              <span>{{ label }}</span>
              <div v-for="(window, index) in settingsSchedule[day]" :key="`${day}-${index}`" class="live-schedule-window">
                <input :id="`live-schedule-${day}-${index}-start`" v-model="window[0]" type="time" :disabled="settingsScheduleMode === 'all-day'" :aria-label="`${label} 时段 ${index + 1} 开始`" />
                <span aria-hidden="true">–</span>
                <input :id="`live-schedule-${day}-${index}-end`" v-model="window[1]" type="time" :disabled="settingsScheduleMode === 'all-day'" :aria-label="`${label} 时段 ${index + 1} 结束`" />
                <button type="button" class="live-schedule-clear" data-mutating :disabled="settingsScheduleMode === 'all-day'" :aria-label="`清空${label}时段${index + 1}`" title="清空该时段" @click="window[0] = ''; window[1] = ''"><i class="fa-solid fa-xmark"></i></button>
              </div>
            </div>
          </div>
          <div id="live-schedule-error" class="live-schedule-error" role="alert">{{ scheduleError }}</div>
        </div>
      </div>
      <div class="modal-footer">
        <button class="btn" id="live-settings-cancel-btn" @click="closeSettingsModal"><i class="fa-solid fa-times"></i> 取消</button>
        <button class="btn btn-primary" id="live-source-save" data-network-required="true" data-mutating :disabled="savingSettings" @click="saveSettingsModal"><i class="fa-solid fa-check"></i> {{ savingSettings ? '保存中…' : '保存设置' }}</button>
      </div>
    </div>
  </div>
</template>
