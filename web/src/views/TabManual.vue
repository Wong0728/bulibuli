<script setup lang="ts">
import { ref, computed, onMounted } from 'vue';
import { useBloggerStore } from '@/stores/blogger';
import { useDownloadStore } from '@/stores/download';
import { useToastStore } from '@/stores/toast';
import { video as videoApi, blogger as bloggerApi } from '@/api';
import type { Series, VideoItem, ManualResolveResult } from '@/api/types';
import { openDrawer } from '@/composables/drawer';

const blogger = useBloggerStore();
const download = useDownloadStore();
const toast = useToastStore();

type Mode = 'submission' | 'series' | 'link';
const mode = ref<Mode>('submission');

const selectedUid = ref<number | null>(null);
const selectedSeriesId = ref<number | null>(null);
const seriesList = ref<Series[]>([]);

const linkInput = ref('');

const videoList = ref<VideoItem[]>([]);
const loadingMore = ref(false);
const hasMore = ref(false);
const currentPage = ref(1);
const totalCount = ref(0);

const linkResult = ref<ManualResolveResult | null>(null);
const selectedEpisodes = ref<Set<string>>(new Set());

onMounted(async () => {
  await blogger.refreshList().catch(() => {});
});

const monitoredBloggers = computed(() => blogger.bloggers);

async function loadSeries() {
  if (!selectedUid.value) { seriesList.value = []; return; }
  try {
    seriesList.value = await blogger.fetchSeries(selectedUid.value);
  } catch (e: any) {
    toast.error(e?.message || '加载合集失败');
  }
}

async function queryVideos() {
  if (!selectedUid.value) { toast.warn('请先选择博主'); return; }
  if (mode.value === 'series' && !selectedSeriesId.value) { toast.warn('请先选择合集'); return; }
  currentPage.value = 1;
  videoList.value = [];
  try {
    if (mode.value === 'series') {
      // 走 /api/blogger/series-videos：后端按合集 ID 取分集列表
      const r: any = (await bloggerApi.seriesVideos(selectedUid.value, selectedSeriesId.value!, { limit: 30 })) || { items: [] };
      videoList.value = (r.items || []).map((it: any) => ({
        bvid: it.bvid, title: it.title || it.display_title || '',
        pic: it.pic, duration: it.duration, cid: it.cid, aid: it.aid,
      }));
      hasMore.value = !!r.has_more;
      totalCount.value = videoList.value.length;
    } else {
      // 走 /api/video/get-videos：后端只支持按投稿分页（limit/offset）
      const limit = 20;
      const r: any = await videoApi.getVideos(selectedUid.value, { limit, offset: 0 });
      videoList.value = r?.items || [];
      hasMore.value = !!r?.has_more;
      totalCount.value = r?.total || 0;
    }
  } catch (e: any) {
    toast.error(e?.message || '查询失败');
  }
}

async function loadMore() {
  if (!hasMore.value || loadingMore.value) return;
  loadingMore.value = true;
  try {
    if (mode.value === 'series') {
      // 合集：offset += 30
      const offset = videoList.value.length;
      const r: any = (await bloggerApi.seriesVideos(selectedUid.value!, selectedSeriesId.value!, { limit: 30, offset })) || { items: [] };
      videoList.value.push(...((r.items || []).map((it: any) => ({
        bvid: it.bvid, title: it.title || it.display_title || '',
        pic: it.pic, duration: it.duration, cid: it.cid, aid: it.aid,
      }))));
      hasMore.value = !!r.has_more;
    } else {
      const limit = 20;
      const offset = videoList.value.length;
      const r: any = await videoApi.getVideos(selectedUid.value!, { limit, offset });
      videoList.value.push(...(r?.items || []));
      hasMore.value = !!r?.has_more;
    }
  } finally {
    loadingMore.value = false;
  }
}

async function resolveLink() {
  if (!linkInput.value.trim()) { toast.warn('请输入链接'); return; }
  try {
    const r: any = await videoApi.resolveLink(linkInput.value);
    if (!r) { linkResult.value = null; return; }
    // 后端 /api/video/resolve 对 BV/AV 只返回 { media: ... }，没有 episodes；
    // 这种情况下 linkResult 设为 null，由模板展示提示信息。
    if (!r.episodes || r.episodes.length === 0) {
      toast.warn('该链接是单视频，请用 BV/AV 直接下载');
      linkResult.value = null;
      return;
    }
    linkResult.value = r as ManualResolveResult;
    // 默认勾选 current_ep_id（如果是 ep 链接）
    const cur = (r as any).current_ep_id;
    if (cur != null) {
      const hit = (r.episodes || []).find((e: any) => e.ep_id === cur);
      selectedEpisodes.value = new Set(hit ? [hit.bvid] : (r.episodes || []).map((e: any) => e.bvid));
    } else {
      selectedEpisodes.value = new Set((r.episodes || []).map((e: any) => e.bvid));
    }
  } catch (e: any) {
    toast.error(e?.message || '解析失败');
    linkResult.value = null;
  }
}

function toggleEpisode(bvid: string) {
  if (selectedEpisodes.value.has(bvid)) selectedEpisodes.value.delete(bvid);
  else selectedEpisodes.value.add(bvid);
  selectedEpisodes.value = new Set(selectedEpisodes.value);
}

function toggleAllEpisodes() {
  if (!linkResult.value?.episodes) return;
  const allBvs = linkResult.value.episodes.map(e => e.bvid);
  const allSelected = allBvs.every(bv => selectedEpisodes.value.has(bv));
  if (allSelected) selectedEpisodes.value = new Set();
  else selectedEpisodes.value = new Set(allBvs);
}

async function batchAddDownload() {
  if (!linkResult.value?.episodes) return;
  const bvs = (linkResult.value.episodes || []).filter(e => selectedEpisodes.value.has(e.bvid));
  if (bvs.length === 0) { toast.warn('请选择至少一个分集'); return; }
  let count = 0;
  for (const ep of bvs) {
    try {
      await download.addTask(ep.bvid, { title: ep.title });
      count++;
    } catch (e: any) { /* 忽略单条失败 */ }
  }
  toast.success(`已添加 ${count}/${bvs.length} 个到下载队列`);
}

async function quickDownload(bvid: string, title: string) {
  try {
    await download.addTask(bvid, { uid: selectedUid.value || undefined, title });
    toast.success('已加入下载队列');
  } catch (e: any) {
    toast.error(e?.message || '加入失败');
  }
}

function showVideoDetail(bvid: string, title: string) {
  openDrawer({ bvid, title, source: 'manual' });
}

function onModeChange() {
  videoList.value = [];
  hasMore.value = false;
  linkResult.value = null;
}

function formatDuration(s?: number) {
  if (!s) return '';
  const m = Math.floor(s / 60);
  const r = s % 60;
  return `${m}:${String(r).padStart(2, '0')}`;
}
</script>

<template>
  <section class="tab-panel">
    <div class="card">
      <div class="card-title">
        <i class="fa-solid fa-search"></i>
        <span>手动查询</span>
        <div class="manual-mode-switch manual-mode-switch-aligned">
          <button :class="['mode-btn', { active: mode === 'submission' }]" data-mode="submission" @click="mode = 'submission'; onModeChange()">
            <i class="fa-solid fa-video"></i> 按投稿
          </button>
          <button :class="['mode-btn', { active: mode === 'series' }]" data-mode="series" @click="mode = 'series'; onModeChange()">
            <i class="fa-solid fa-layer-group"></i> 按合集
          </button>
          <button :class="['mode-btn', { active: mode === 'link' }]" data-mode="link" @click="mode = 'link'; onModeChange()">
            <i class="fa-solid fa-link"></i> 按链接
          </button>
        </div>
      </div>

      <div v-if="mode !== 'link'" class="uid-input-group">
        <div class="form-group">
          <label for="uid-history-select">选择博主</label>
          <select id="uid-history-select" v-model="selectedUid" class="blogger-select" @change="loadSeries">
            <option :value="null">-- 从已添加博主中选择 --</option>
            <option v-for="b in monitoredBloggers" :key="b.uid" :value="b.uid">{{ b.name }} ({{ b.uid }})</option>
          </select>
        </div>
        <div v-if="mode === 'series'" class="form-group manual-series-select-group">
          <label for="manual-series-select">选择合集</label>
          <select id="manual-series-select" v-model="selectedSeriesId" class="blogger-select">
            <option :value="null">-- 请先选择博主 --</option>
            <option v-for="s in seriesList" :key="s.series_id" :value="s.series_id">{{ s.title }} ({{ s.count || 0 }}P)</option>
          </select>
        </div>
        <button class="btn btn-primary" id="manual-query-btn" @click="queryVideos">
          <i class="fa-solid fa-play-circle"></i> 查询最新视频
        </button>
      </div>

      <div v-if="mode === 'link'" id="manual-link-input-group" class="uid-input-group">
        <div class="form-group form-full">
          <label for="manual-link-input">输入番剧 / 课程链接</label>
          <input id="manual-link-input" v-model="linkInput" type="text" class="form-control"
                 placeholder="支持 ep / ss / fp 链接，如 https://www.bilibili.com/bangumi/play/ep123" />
          <div class="form-note">番剧（ep/ss）与课程（fp）链接会解析出分集列表，勾选后批量下载；普通视频（BV/AV）链接走单视频流程。</div>
        </div>
        <button class="btn btn-primary" id="manual-resolve-btn" data-action="resolve-link" @click="resolveLink">
          <i class="fa-solid fa-magnifying-glass"></i> 解析链接
        </button>
      </div>
    </div>

    <div id="manual-result"></div>

    <!-- 视频列表（与原版结构同构：video-card 网格） -->
    <div v-if="videoList.length" class="card">
      <div class="card-title">
        <span><i class="fa-solid fa-list"></i> 查询结果（共 {{ totalCount }} 条）</span>
      </div>
      <div class="manual-result-grid">
        <div v-for="v in videoList" :key="v.bvid" class="video-card">
          <div class="video-card-top">
            <img v-if="v.pic" :src="v.pic" class="video-card-cover" />
            <div class="video-card-info">
              <div class="video-card-title">{{ v.title }}</div>
              <div class="video-card-meta">
                <span>{{ formatDuration(v.duration) }}</span>
                <span v-if="v.pubdate">{{ new Date(v.pubdate * 1000).toLocaleDateString() }}</span>
              </div>
            </div>
          </div>
          <div class="video-card-actions">
            <button class="btn btn-sm btn-primary" @click="quickDownload(v.bvid, v.title)">
              <i class="fa-solid fa-download"></i> 下载
            </button>
            <button class="btn btn-sm" @click="showVideoDetail(v.bvid, v.title)">
              <i class="fa-solid fa-info-circle"></i> 详情
            </button>
          </div>
        </div>
      </div>
      <div v-if="hasMore" id="manual-load-more" class="manual-load-more">
        <button class="btn" :disabled="loadingMore" @click="loadMore">
          {{ loadingMore ? '加载中…' : '加载更多' }}
        </button>
      </div>
    </div>

    <!-- 链接解析结果 -->
    <div v-if="linkResult && linkResult.episodes?.length" class="card">
      <div class="card-title">
        <span><i class="fa-solid fa-list-ul"></i> {{ linkResult.season_title || '解析结果' }}（{{ linkResult.episodes.length }} 集）</span>
        <button class="btn btn-sm" style="margin-left: auto;" @click="toggleAllEpisodes">全选/反选</button>
        <button class="btn btn-sm btn-primary" @click="batchAddDownload">
          <i class="fa-solid fa-download"></i> 批量下载
        </button>
      </div>
      <div class="manual-result-grid">
        <div v-for="ep in linkResult.episodes" :key="(ep as any).bvid || (ep as any).ep_id" class="video-card">
          <div class="video-card-top">
            <img v-if="ep.pic" :src="ep.pic" class="video-card-cover" />
            <div class="video-card-info">
              <div class="video-card-title">{{ ep.title }}</div>
              <div class="video-card-meta">
                <span>{{ formatDuration(ep.duration) }}</span>
              </div>
            </div>
          </div>
          <div class="video-card-actions">
            <label class="choice-row">
              <span>选择下载</span>
              <span class="toggle-switch">
                <input type="checkbox" :checked="selectedEpisodes.has(ep.bvid)" @change="toggleEpisode(ep.bvid)" />
                <span class="slider"></span>
              </span>
            </label>
          </div>
        </div>
      </div>
    </div>
  </section>
</template>
