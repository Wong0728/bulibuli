<script setup lang="ts">
import { computed } from 'vue';

export interface BloggerConfigFormModel {
  uid: string;
  name: string;
  min_interval: number;
  max_interval: number;
  all_day: boolean;
  active_windows: string[];
  download_video: boolean;
  download_danmaku: boolean;
  download_comments: boolean;
  download_cover: boolean;
  burn_danmaku: boolean;
  burn_subtitle: boolean;
  series_filter_regex: string;
  start_monitoring: boolean;
}

const props = withDefaults(defineProps<{
  modelValue: BloggerConfigFormModel;
  uidReadonly?: boolean;
}>(), { uidReadonly: false });
const emit = defineEmits<{ 'update:modelValue': [BloggerConfigFormModel] }>();

const form = computed(() => props.modelValue);

function update<K extends keyof BloggerConfigFormModel>(key: K, value: BloggerConfigFormModel[K]) {
  emit('update:modelValue', { ...props.modelValue, [key]: value });
}

function updateWindow(index: number, part: 'start' | 'end', value: string) {
  const next = props.modelValue.active_windows.slice();
  const [start = '', end = ''] = next[index]?.split('-') ?? [];
  next[index] = `${part === 'start' ? value : start}-${part === 'end' ? value : end}`;
  update('active_windows', next);
}

function addWindow(start = '18:00', end = '23:00') {
  if (form.value.active_windows.length >= 6) return;
  update('active_windows', [...form.value.active_windows, `${start}-${end}`]);
}

function removeWindow(index: number) {
  update('active_windows', form.value.active_windows.filter((_, i) => i !== index));
}

function toggleAllDay(value: boolean) {
  if (!value && form.value.active_windows.length === 0) {
    emit('update:modelValue', { ...props.modelValue, all_day: false, active_windows: ['18:00-23:00'] });
    return;
  }
  update('all_day', value);
}

function isNextDay(value: string) {
  const [start, end] = value.split('-');
  return Boolean(start && end && end < start);
}

function windowPart(value: string, part: 'start' | 'end') {
  return value.split('-')[part === 'start' ? 0 : 1] || '';
}
</script>

<template>
  <div class="form-section">
    <div class="form-group">
      <label for="blogger-config-uid"><i class="fa-solid fa-id-card"></i> 博主 UID</label>
      <input id="blogger-config-uid" class="form-control" inputmode="numeric" autocomplete="off"
             :value="form.uid" :readonly="uidReadonly"
             placeholder="请输入数字 UID"
             @input="update('uid', ($event.target as HTMLInputElement).value.replace(/[^0-9]/g, ''))" />
    </div>
    <div class="form-group">
      <label for="blogger-config-name"><i class="fa-solid fa-user-tag"></i> 博主备注名</label>
      <input id="blogger-config-name" class="form-control" :value="form.name"
             placeholder="可选，留空将使用 B 站昵称"
             @input="update('name', ($event.target as HTMLInputElement).value)" />
    </div>
    <div class="form-group">
      <label for="blogger-config-min"><i class="fa-solid fa-sync-alt"></i> 最小检查间隔（秒）</label>
      <input id="blogger-config-min" type="number" class="form-control" min="30" max="3600" :value="form.min_interval"
             @input="update('min_interval', Number(($event.target as HTMLInputElement).value))" />
      <div class="form-note">随机检查范围的下限，建议 60 秒。</div>
    </div>
    <div class="form-group">
      <label for="blogger-config-max"><i class="fa-solid fa-sync-alt"></i> 最大检查间隔（秒）</label>
      <input id="blogger-config-max" type="number" class="form-control" min="30" max="7200" :value="form.max_interval"
             @input="update('max_interval', Number(($event.target as HTMLInputElement).value))" />
      <div class="form-note">随机检查范围的上限，建议 300 秒。</div>
    </div>

    <div class="form-divider"><span>监测时段</span></div>
    <div class="form-group form-full active-window-editor">
      <label class="choice-row">
        <span><strong>全天监测</strong><small>关闭后可设置最多 6 个监测窗口，使用服务器本地时区。</small></span>
        <span class="toggle-switch">
          <input type="checkbox" :checked="form.all_day" @change="toggleAllDay(($event.target as HTMLInputElement).checked)" />
          <span class="slider"></span>
        </span>
      </label>
      <div v-if="!form.all_day" class="active-window-list">
        <div v-for="(window, index) in form.active_windows" :key="index" class="active-window-row">
          <label class="active-window-time">
            <span>开始</span>
            <span class="time-input-shell"><i class="fa-regular fa-clock"></i><input type="time" step="300" :value="windowPart(window, 'start')" @input="updateWindow(index, 'start', ($event.target as HTMLInputElement).value)" /></span>
          </label>
          <span class="active-window-separator" aria-hidden="true">至</span>
          <label class="active-window-time">
            <span>结束</span>
            <span class="time-input-shell"><i class="fa-regular fa-clock"></i><input type="time" step="300" :value="windowPart(window, 'end')" @input="updateWindow(index, 'end', ($event.target as HTMLInputElement).value)" /></span>
          </label>
          <span v-if="isNextDay(window)" class="active-window-next-day">次日</span>
          <button type="button" class="active-window-remove" aria-label="删除该时段" title="删除该时段" @click="removeWindow(index)">
            <i class="fa-solid fa-trash"></i>
          </button>
        </div>
        <div class="active-window-controls">
          <button type="button" class="btn" :disabled="form.active_windows.length >= 6" @click="addWindow()"><i class="fa-solid fa-plus"></i> 添加时段</button>
          <span class="active-window-presets">
            快捷：
            <button type="button" class="btn btn-sm" @click="addWindow('08:00', '18:00')">白天</button>
            <button type="button" class="btn btn-sm" @click="addWindow('18:00', '23:00')">晚间</button>
            <button type="button" class="btn btn-sm" @click="addWindow('22:00', '02:00')">跨夜</button>
          </span>
        </div>
      </div>
    </div>

    <div class="form-divider"><span>下载策略</span></div>
    <div class="form-group form-full policy-grid">
      <label v-for="item in [
        ['download_video', '下载视频'], ['download_danmaku', '下载弹幕'],
        ['download_comments', '下载评论'], ['download_cover', '下载封面'],
        ['burn_danmaku', '自动烧录弹幕'], ['burn_subtitle', '自动烧录 CC 字幕'],
      ]" :key="item[0]" class="choice-row">
        <span>{{ item[1] }}</span>
        <span class="toggle-switch">
          <input type="checkbox" :checked="form[item[0] as keyof BloggerConfigFormModel] as boolean"
                 @change="update(item[0] as keyof BloggerConfigFormModel, ($event.target as HTMLInputElement).checked as never)" />
          <span class="slider"></span>
        </span>
      </label>
    </div>
    <div class="form-group form-full">
      <label for="blogger-config-regex"><i class="fa-solid fa-filter"></i> 合集白名单正则</label>
      <input id="blogger-config-regex" class="form-control" :value="form.series_filter_regex"
             placeholder="留空=全部合集；例如：.*精选.*"
             @input="update('series_filter_regex', ($event.target as HTMLInputElement).value)" />
    </div>
    <div class="form-group form-full">
      <label class="choice-row choice-row-emphasis">
        <span><strong>保存后立即启用监控</strong><small>若当前在设定时段外，将显示“时段外暂停”并在下一窗口自动恢复。</small></span>
        <span class="toggle-switch">
          <input type="checkbox" :checked="form.start_monitoring" @change="update('start_monitoring', ($event.target as HTMLInputElement).checked)" />
          <span class="slider"></span>
        </span>
      </label>
    </div>
  </div>
</template>
