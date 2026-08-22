<script setup lang="ts">
import { ref, computed, onMounted } from 'vue';
import { useBloggerStore } from '@/stores/blogger';
import { useDownloadStore } from '@/stores/download';
import { useToastStore } from '@/stores/toast';
import { useSettingsStore } from '@/stores/settings';
import { video as videoApi, blogger as bloggerApi } from '@/api';
import type { Series, VideoItem, ManualResolveResult } from '@/api/types';
import { openDrawer } from '@/composables/drawer';
// 需要消费后端 message（“已提交 X/Y 个分P下载”）时用 Full 变体，在白名单文件内本地封装。
import { postFull } from '../api/client';

const blogger = useBloggerStore();
const download = useDownloadStore();
const toast = useToastStore();
const settings = useSettingsStore();

function imageUrl(url?: string) {
  if (!url) return '';
  const normalized = url.startsWith('//') ? `https:${url}` : url;
  return videoApi.proxyImage(normalized);
}
function imageError(event: Event) {
  const image = event.target as HTMLImageElement;
  image.hidden = true;
  image.nextElementSibling?.removeAttribute('hidden');
}

const qualityOptions = [
  { qn: 125, label: 'HDR', tag: 'HDR' },
  { qn: 120, label: '4K 超清', tag: '4K' },
  { qn: 116, label: '1080P60', tag: '60帧' },
  { qn: 112, label: '1080P+ 高码率', tag: '高码' },
  { qn: 80, label: '1080P 高清', tag: '1080P' },
  { qn: 74, label: '720P60', tag: '60帧' },
  { qn: 64, label: '720P 高清', tag: '720P' },
  { qn: 32, label: '480P 清晰', tag: '480P' },
  { qn: 16, label: '360P 流畅', tag: '360P' },
];

type Mode = 'submission' | 'series' | 'link';
const mode = ref<Mode>('submission');

const selectedUid = ref<number | null>(null);
const selectedSeriesId = ref<number | null>(null);
const seriesList = ref<Series[]>([]);

const linkInput = ref('');

const videoList = ref<VideoItem[]>([]);
const loadingMore = ref(false);
const hasMore = ref(false);
const totalCount = ref(0);
// 分页游标（对齐老框架 _state.manualQueryOffset / manualQueryLimit）：
// 优先采用后端返回的 data.offset，加载更多时在此基础上叠加。
const queryOffset = ref(0);
const queryLimit = ref(20);

const linkResult = ref<ManualResolveResult | null>(null);
const selectedEpisodes = ref<Set<string>>(new Set());
const selectedQuality = ref(80);
const seriesLoading = ref(false);
type ResultState = 'idle' | 'loading' | 'success' | 'empty' | 'error';
const resultState = ref<ResultState>('idle');
const resultMessage = ref('');
const resultHint = ref('');

onMounted(async () => {
  await blogger.refreshSaved().catch((e: any) => toast.error(e?.message || '已添加博主加载失败'));
});

const monitoredBloggers = computed(() => blogger.savedBloggers);
const querying = ref(false);
const resolving = ref(false);
const batching = ref(false);
const manualLimit = computed(() => Math.min(Math.max(Number(settings.settings.manual_query_limit) || 20, 1), 50));
// series-videos 后端硬校验 limit 1..=30（src/api/blogger/discover.rs），
// 设置 ≥31 时合集分支必须单独钳制，否则首查和加载更多恒 400。
const seriesLimit = computed(() => Math.min(manualLimit.value, 30));
const selectedCollectionType = computed(() => {
  const selected = seriesList.value.find(s => Number(s.series_id) === Number(selectedSeriesId.value));
  return selected?.type === 'season' ? 'season' : 'series';
});
const queryButtonLabel = computed(() => mode.value === 'series' ? '加载该合集视频' : '查询最新视频');

type ManualVideoItem = VideoItem & { default_quality?: number };
function normalizeVideo(item: any): ManualVideoItem {
  const created = Number(item?.created ?? item?.pubdate ?? item?.pub_timestamp ?? 0);
  const rawDuration = item?.duration ?? item?.length;
  // 播放量：投稿/合集列表用顶层 play；BV 链接解析（/api/video/info）字段在 stat.view（对齐老框架 renderResolvedNormalVideo）。
  const play = item?.play != null ? Number(item.play)
    : (item?.stat?.view != null ? Number(item.stat.view) : undefined);
  return {
    bvid: String(item?.bvid || ''),
    aid: item?.aid == null ? undefined : Number(item.aid),
    title: item?.title || item?.display_title || item?.long_title || '未知标题',
    pic: item?.pic || item?.cover || undefined,
    duration: typeof rawDuration === 'number' ? rawDuration : undefined,
    length: typeof rawDuration === 'string' ? rawDuration : undefined,
    pubdate: created || undefined,
    created: created || undefined,
    play,
    cid: item?.cid == null ? undefined : Number(item.cid),
    pages: item?.pages,
    is_charging_arc: Boolean(item?.is_charging_arc ?? item?.rights?.ugc_pay ?? item?.rights?.pay),
    // 服务端设置附带的首选画质（对齐老框架 manualQueryVideos[].default_quality）。
    default_quality: item?.default_quality == null ? undefined : Number(item.default_quality),
  };
}

function clearResults() {
  videoList.value = [];
  hasMore.value = false;
  totalCount.value = 0;
  linkResult.value = null;
  selectedEpisodes.value = new Set();
  resultState.value = 'idle';
  resultMessage.value = '';
  resultHint.value = '';
}

async function loadSeries() {
  selectedSeriesId.value = null;
  seriesList.value = [];
  if (!selectedUid.value) return;
  seriesLoading.value = true;
  try {
    seriesList.value = await blogger.fetchSeries(selectedUid.value);
  } catch (e: any) {
    toast.error(e?.message || '加载合集失败');
  } finally {
    seriesLoading.value = false;
  }
}

async function onUidChange() {
  if (mode.value === 'series') await loadSeries();
  else {
    selectedSeriesId.value = null;
    seriesList.value = [];
  }
}

async function switchMode(nextMode: Mode) {
  mode.value = nextMode;
  clearResults();
  if (nextMode === 'series' && selectedUid.value) await loadSeries();
}

async function queryVideos() {
  if (querying.value) return;
  if (!selectedUid.value) { toast.warn('请先选择博主'); return; }
  if (mode.value === 'series' && !selectedSeriesId.value) { toast.warn('请先选择合集'); return; }
  querying.value = true;
  resultState.value = 'loading';
  resultMessage.value = '正在请求B站...';
  resultHint.value = '';
  videoList.value = [];
  linkResult.value = null;
  try {
    // 对齐老框架 doManualQuery：limit 读设置项 setting-manual-query-limit（clamp 1..50），
    // 投稿查询直接用该值；合集接口后端上限 30，超设置部分由 seriesLimit 单独钳制。
    const limit = mode.value === 'series' ? seriesLimit.value : manualLimit.value;
    queryLimit.value = limit;
    queryOffset.value = 0;
    if (mode.value === 'series') {
      // 走 /api/blogger/series-videos：后端按合集 ID 取分集列表
      const r: any = (await bloggerApi.seriesVideos(selectedUid.value, selectedSeriesId.value!, {
        collection_type: selectedCollectionType.value,
        limit,
        offset: 0,
      })) || { videos: [] };
      const videos = r.videos || [];
      videoList.value = videos.map(normalizeVideo);
      queryOffset.value = Number.isFinite(Number(r.offset)) ? Number(r.offset) : 0;
      hasMore.value = r.has_more === true || (r.has_more !== false && videos.length >= limit);
      totalCount.value = Number(r.total ?? videoList.value.length);
    } else {
      // 走 /api/video/get-videos：后端只支持按投稿分页（limit/offset）
      const r: any = await videoApi.getVideos(selectedUid.value, { limit, offset: 0 });
      const videos = r?.videos || [];
      videoList.value = videos.map(normalizeVideo);
      queryOffset.value = Number.isFinite(Number(r?.offset)) ? Number(r.offset) : 0;
      hasMore.value = r?.has_more === true || (r?.has_more !== false && videos.length >= limit);
      totalCount.value = Number(r?.total ?? videoList.value.length);
    }
    resultState.value = videoList.value.length ? 'success' : 'empty';
    if (!videoList.value.length) resultMessage.value = '暂无视频';
  } catch (e: any) {
    resultState.value = 'error';
    resultMessage.value = e?.message || '查询失败';
    toast.error(e?.message || '查询失败');
  } finally {
    querying.value = false;
  }
}

async function loadMore() {
  if (!hasMore.value || loadingMore.value) return;
  loadingMore.value = true;
  try {
    // 对齐老框架 loadMoreManualQuery：nextOffset = 当前游标 + 当次 limit，成功后回写后端 data.offset。
    const limit = queryLimit.value;
    const nextOffset = queryOffset.value + limit;
    if (mode.value === 'series') {
      const r: any = (await bloggerApi.seriesVideos(selectedUid.value!, selectedSeriesId.value!, {
        collection_type: selectedCollectionType.value,
        limit,
        offset: nextOffset,
      })) || { videos: [] };
      const videos = r.videos || [];
      videoList.value.push(...videos.map(normalizeVideo));
      queryOffset.value = Number.isFinite(Number(r.offset)) ? Number(r.offset) : nextOffset;
      hasMore.value = r.has_more === true || (r.has_more !== false && videos.length >= limit);
      totalCount.value = Number(r.total ?? videoList.value.length);
    } else {
      const r: any = await videoApi.getVideos(selectedUid.value!, { limit, offset: nextOffset });
      const videos = r?.videos || [];
      videoList.value.push(...videos.map(normalizeVideo));
      queryOffset.value = Number.isFinite(Number(r?.offset)) ? Number(r.offset) : nextOffset;
      hasMore.value = r?.has_more === true || (r?.has_more !== false && videos.length >= limit);
      totalCount.value = Number(r?.total ?? videoList.value.length);
    }
  } catch (e: any) {
    toast.error(e?.message || '加载更多失败');
  } finally {
    loadingMore.value = false;
  }
}

async function resolveLink() {
  if (resolving.value) return;
  if (!linkInput.value.trim()) { toast.warn('请输入链接'); return; }
  resolving.value = true;
  videoList.value = [];
  linkResult.value = null;
  hasMore.value = false;
  resultState.value = 'loading';
  resultMessage.value = '正在解析链接...';
  resultHint.value = '';
  try {
    const r: any = await videoApi.resolveLink(linkInput.value);
    if (!r) throw new Error('解析结果为空');
    // 普通 BV 链接沿用旧版的单视频卡片流程，不再只提示“请直接下载”却不给操作入口。
    if (!r.episodes || r.episodes.length === 0) {
      const media = r.media || {};
      if (media.type === 'video_bv' && media.id) {
        const info: any = await videoApi.info(String(media.id));
        if (!info?.bvid) throw new Error('获取视频信息失败');
        videoList.value = [normalizeVideo({ ...info, created: info.pub_timestamp })];
        totalCount.value = 1;
        hasMore.value = false;
        linkResult.value = null;
        resultState.value = 'success';
        return;
      }
      if (r.pay_blocked) {
        resultState.value = 'error';
        resultMessage.value = r.message || '当前账号无权限访问该内容';
        resultHint.value = r.media_type === 'pgc'
          ? '该番剧可能需要大会员专享或存在区域限制'
          : '该课程可能需要购买后才能下载';
      } else {
        resultState.value = 'empty';
        resultMessage.value = media.type === 'video_av'
          ? '暂不支持 AV 链接，请使用 BV 链接（可在视频页地址栏复制）'
          : '未能识别的链接类型';
      }
      linkResult.value = null;
      return;
    }
    linkResult.value = r as ManualResolveResult;
    selectedQuality.value = Number(r.default_quality) || 80;
    // 默认勾选 current_ep_id（如果是 ep 链接）
    const cur = (r as any).current_ep_id;
    if (cur != null) {
      const hit = (r.episodes || []).find((e: any) => e.ep_id === cur);
      selectedEpisodes.value = new Set(hit ? [hit.bvid] : (r.episodes || []).map((e: any) => e.bvid));
    } else {
      selectedEpisodes.value = new Set((r.episodes || []).map((e: any) => e.bvid));
    }
    resultState.value = 'success';
  } catch (e: any) {
    resultState.value = 'error';
    resultMessage.value = e?.message || '解析失败';
    toast.error(e?.message || '解析失败');
    linkResult.value = null;
  } finally {
    resolving.value = false;
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

function selectQuality(qn: number) {
  selectedQuality.value = qn;
}

async function batchAddDownload() {
  if (batching.value) return;
  if (!linkResult.value?.episodes) return;
  const mediaType = linkResult.value.media_type;
  if (!mediaType) return;
  // 对齐老框架 startSeasonDownload：page 用完整分集列表的原始序号（勾选第 3、5 集仍提交 page:3,5），
  // 不按筛选后顺序重编号。
  const pages = (linkResult.value.episodes || [])
    .map((ep, idx) => ({ ep, idx }))
    .filter(({ ep }) => selectedEpisodes.value.has(ep.bvid))
    .map(({ ep, idx }) => {
      const page: Record<string, any> = {
        cid: Number(ep.cid) || 0,
        page: idx + 1,
        part: ep.display_title || ep.title || `ep${ep.ep_id}`,
        ep_id: Number(ep.ep_id),
        aid: Number(ep.aid) || 0,
      };
      const bvid = ep.bvid || '';
      // 仅在 bvid 非空时携带；为空时后端会回退到顶层 bvid（PageSelector.bvid = None）。
      if (bvid) page.bvid = bvid;
      return page;
    });
  if (pages.length === 0) { toast.warn('请选择至少一个分集'); return; }
  // 番剧需要每集携带 bvid；课程需要每集携带 aid
  for (const p of pages) {
    if (mediaType === 'pgc' && !p.bvid) {
      toast.error(`第 ${p.page} 集缺少 bvid，无法下载`);
      return;
    }
    if (mediaType === 'cheese' && (!p.aid || p.aid === 0)) {
      toast.error(`第 ${p.page} 集缺少 aid，无法下载`);
      return;
    }
  }
  // 番剧/课程用任意一集的 bvid 作为请求顶层 bvid；后端仅用于日志，实际取流按 pages 中的 ep_id 处理。
  const fallbackBvid = pages.find(p => p.bvid)?.bvid
    || (linkResult.value.episodes || []).find(e => e.bvid)?.bvid
    || '';
  if (!fallbackBvid) { toast.error('无法确定下载任务的 bvid'); return; }
  batching.value = true;
  try {
    const { data, message } = await postFull<any>('/api/download/start', {
      bvid: fallbackBvid,
      qn: selectedQuality.value,
      media_type: mediaType,
      season_title: linkResult.value.season_title || '',
      pages,
    });
    // 对齐老框架：toast 优先后端 result.message，成功后刷新下载列表。
    const okCount = (data as any)?.ok_count ?? pages.length;
    const total = (data as any)?.total ?? pages.length;
    toast.success(message || `已提交 ${okCount}/${total} 个分集下载`);
    void download.refreshStatus();
  } catch (e: any) { toast.error(e?.message || '下载请求失败'); }
  finally { batching.value = false; }
}

function showVideoDetail(bvid: string, title: string) {
  // 手动查询抽屉与已下载抽屉是两套渲染（老框架 renderDrawerContentForManualQuery）；
  // 把查询结果条目整体传入，供封面/时长/发布时间等展示。
  const entry = videoList.value.find(v => v.bvid === bvid);
  openDrawer({ bvid, title, source: 'manual', manualVideo: entry ? { ...entry } : undefined });
}

const selectedEpisodeCount = computed(() => {
  if (!linkResult.value?.episodes) return 0;
  return linkResult.value.episodes.filter(ep => selectedEpisodes.value.has(ep.bvid)).length;
});

function formatDuration(s?: number | string) {
  if (typeof s === 'string') return s;
  if (!s || !Number.isFinite(s)) return '';
  const m = Math.floor(s / 60);
  const r = s % 60;
  return `${m}:${String(r).padStart(2, '0')}`;
}

function formatDate(timestamp?: number) {
  return timestamp ? new Date(timestamp * 1000).toLocaleDateString('zh-CN') : '--';
}

function formatFans(n?: number) {
  n = Number(n) || 0;
  if (n >= 10000) return (n / 10000).toFixed(1) + '万';
  return n.toString();
}

function optionMeta(b: any) {
  return `UID: ${b.uid} · Lv${b.level || 0} · 粉丝 ${formatFans(b.fans)}`;
}
</script>

<template>
  <section class="tab-panel">
    <div class="card">
      <div class="card-title">
        <i class="fa-solid fa-search"></i>
        <span>手动查询</span>
        <div class="manual-mode-switch manual-mode-switch-aligned" role="group" aria-label="查询方式">
          <button type="button" :class="['mode-btn', { active: mode === 'submission' }]" data-mode="submission"
                  :aria-pressed="mode === 'submission'" @click="switchMode('submission')">
            <i class="fa-solid fa-video"></i> 按投稿
          </button>
          <button type="button" :class="['mode-btn', { active: mode === 'series' }]" data-mode="series"
                  :aria-pressed="mode === 'series'" @click="switchMode('series')">
            <i class="fa-solid fa-layer-group"></i> 按合集
          </button>
          <button type="button" :class="['mode-btn', { active: mode === 'link' }]" data-mode="link"
                  :aria-pressed="mode === 'link'" @click="switchMode('link')">
            <i class="fa-solid fa-link"></i> 按链接
          </button>
        </div>
      </div>

      <div v-if="mode !== 'link'" class="uid-input-group">
        <div class="form-group">
          <label for="uid-history-select">选择博主</label>
          <select id="uid-history-select" v-model="selectedUid" class="blogger-select" @change="onUidChange">
            <option :value="null">-- 从已添加博主中选择 --</option>
            <option v-for="b in monitoredBloggers" :key="b.uid" :value="b.uid">
              <img v-if="b.face" class="opt-avatar" :src="imageUrl(b.face)" alt="" />
              <span class="opt-info">
                <span class="opt-name">{{ b.name }}</span>
                <span class="opt-meta">{{ optionMeta(b) }}</span>
              </span>
            </option>
          </select>
        </div>
        <div v-if="mode === 'series'" id="manual-series-select-group" class="form-group manual-series-select-group">
          <label for="manual-series-select">选择合集</label>
          <select id="manual-series-select" v-model="selectedSeriesId" class="blogger-select">
            <option v-if="!selectedUid" :value="null">-- 请先选择博主 --</option>
            <option v-else-if="seriesLoading" :value="null">—— 正在加载合集 ——</option>
            <option v-else-if="!seriesList.length" :value="null">该博主暂无合集</option>
            <option v-for="s in seriesList" :key="s.series_id" :value="s.series_id">
              {{ s.title || s.name }}{{ s.type === 'season' ? ' [合集]' : ' [系列]' }} ({{ s.count || 0 }}P)
            </option>
          </select>
        </div>
        <button type="button" class="btn btn-primary" id="manual-query-btn" data-network-required="true"
                :disabled="querying" @click="queryVideos">
          <span v-if="querying" class="loading"></span>
          <i v-else :class="mode === 'series' ? 'fa-solid fa-layer-group' : 'fa-solid fa-play-circle'"></i>
          {{ querying ? '查询中' : queryButtonLabel }}
        </button>
      </div>

      <div v-if="mode === 'link'" id="manual-link-input-group" class="uid-input-group">
        <div class="form-group form-full">
          <label for="manual-link-input">输入番剧 / 课程链接</label>
          <input id="manual-link-input" v-model="linkInput" type="text" class="form-control"
                 placeholder="支持 ep / ss / fp 链接，如 https://www.bilibili.com/bangumi/play/ep123" />
          <div class="form-note">番剧（ep/ss）与课程（fp）链接会解析出分集列表，勾选后批量下载；普通视频（BV/AV）链接走单视频流程。</div>
        </div>
        <button type="button" class="btn btn-primary" id="manual-resolve-btn" data-action="resolve-link"
                data-network-required="true" :disabled="resolving" @click="resolveLink">
          <span v-if="resolving" class="loading"></span>
          <i v-else class="fa-solid fa-magnifying-glass"></i>
          {{ resolving ? '解析中' : '解析链接' }}
        </button>
      </div>
    </div>

    <div id="manual-result">
      <div v-if="resultState === 'loading'" class="card empty-state">
        <i class="fa-solid fa-spinner fa-spin fa-2x mb-md"></i>
        <p>{{ resultMessage }}</p>
      </div>
      <div v-else-if="resultState === 'error'" class="card empty-state status-error">
        <i :class="['fa-solid', resultHint ? 'fa-lock' : 'fa-exclamation-triangle', 'fa-2x', 'mb-md']"></i>
        <p>{{ resultMessage }}</p>
        <p v-if="resultHint" class="empty-hint">{{ resultHint }}</p>
      </div>
      <div v-else-if="resultState === 'empty'" class="card empty-state">
        <i class="fa-solid fa-inbox fa-2x mb-md"></i>
        <p>{{ resultMessage }}</p>
      </div>

      <div v-else-if="videoList.length" class="video-grid" id="manual-video-grid">
        <div v-for="v in videoList" :key="v.bvid" class="video-card-grid not-downloaded"
             data-action="open-manual-video" :data-bvid="v.bvid"
             @click="showVideoDetail(v.bvid, v.title)">
          <div class="video-card-thumb">
            <template v-if="v.pic">
              <img :src="imageUrl(v.pic)" alt="" loading="lazy" @error="imageError" />
              <div class="video-thumb-fallback" hidden><i class="fa-solid fa-video"></i></div>
            </template>
            <span v-else class="video-thumb-fallback"><i class="fa-solid fa-video"></i></span>
            <span v-if="v.is_charging_arc" class="video-card-badge pay" title="充电/付费专属">
              <i class="fa-solid fa-coins"></i>
            </span>
            <span v-else class="video-card-badge not-downloaded">未下载</span>
            <span v-if="v.duration || v.length" class="video-card-duration">{{ formatDuration(v.duration || v.length) }}</span>
          </div>
          <div class="video-card-body">
            <div class="video-card-title" :title="v.title">{{ v.title }}</div>
            <div class="video-card-meta">
              <div class="video-card-meta-left">
                <span class="video-card-meta-item">
                  <i class="fa-solid fa-calendar-alt"></i> {{ formatDate(v.created || v.pubdate) }}
                </span>
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>

    <div v-if="videoList.length && mode !== 'link'" id="manual-load-more" class="manual-load-more">
      <button v-if="hasMore" type="button" class="btn btn-ghost" :disabled="loadingMore" @click="loadMore">
        <span v-if="loadingMore" class="loading"></span>
        <i v-else class="fa-solid fa-chevron-down"></i>
        {{ loadingMore ? '加载中' : '加载更多' }}
      </button>
      <span v-else class="manual-no-more">没有更多了</span>
    </div>

    <!-- 番剧 / 课程链接解析结果，与旧版抽屉式信息布局保持一致。 -->
    <div v-if="linkResult && linkResult.episodes?.length" class="card manual-season-result">
      <div class="drawer-preview">
        <template v-if="linkResult.cover">
          <img :src="imageUrl(linkResult.cover)" alt="" @error="imageError" />
          <div class="video-thumb-fallback" hidden><i class="fa-solid fa-film"></i></div>
        </template>
        <div v-else class="video-thumb-fallback"><i class="fa-solid fa-film"></i></div>
      </div>
      <div class="drawer-info-row drawer-info-row-spaced">
        <div class="drawer-info-item">
          <span class="drawer-info-label">类型</span>
          <span class="drawer-info-value">{{ linkResult.media_type === 'pgc' ? '番剧' : '课程' }}</span>
        </div>
        <div class="drawer-info-item">
          <span class="drawer-info-label">分集数</span>
          <span class="drawer-info-value">{{ linkResult.episodes.length }}</span>
        </div>
        <div class="drawer-info-item manual-season-title">
          <span class="drawer-info-label">季标题</span>
          <span class="drawer-info-value" :title="linkResult.season_title || '未知季'">{{ linkResult.season_title || '未知季' }}</span>
        </div>
      </div>

      <div class="drawer-section drawer-section-spaced">
        <div class="drawer-section-title">画质选择</div>
        <div class="quality-pills" id="quality-pills-container">
          <button v-for="q in qualityOptions" :key="q.qn" type="button" class="quality-pill"
                  :class="{ active: selectedQuality === q.qn }" @click="selectQuality(q.qn)">
            {{ q.label }} <span class="quality-pill-tag">{{ q.tag }}</span>
          </button>
        </div>
      </div>

      <div class="drawer-section">
        <div class="drawer-pages" id="season-episodes-section">
          <div class="drawer-pages-header">
            <span class="drawer-pages-label">分集选择（共 {{ linkResult.episodes.length }} 集）</span>
            <button type="button" class="btn btn-ghost btn-sm" @click="toggleAllEpisodes">全选/全不选</button>
          </div>
          <div class="drawer-pages-list">
            <label v-for="(ep, index) in linkResult.episodes" :key="ep.bvid || ep.ep_id || index" class="drawer-page-item manual-episode-item">
              <input type="checkbox" :checked="selectedEpisodes.has(ep.bvid)" @change="toggleEpisode(ep.bvid)" />
              <span class="drawer-page-index">{{ ep.section_title || `P${index + 1}` }}</span>
              <span class="drawer-page-title" :title="ep.display_title || ep.title">{{ ep.display_title || ep.title || `ep${ep.ep_id || index + 1}` }}</span>
              <span v-if="ep.badge" class="manual-episode-badge">{{ ep.badge }}</span>
            </label>
          </div>
        </div>
        <div class="drawer-actions manual-season-actions">
          <button type="button" class="drawer-btn drawer-btn-primary" data-mutating
                  :disabled="batching || selectedEpisodeCount === 0" @click="batchAddDownload">
            <span v-if="batching" class="loading"></span>
            <i v-else class="fa-solid fa-download"></i>
            {{ batching ? '提交中' : (linkResult.current_ep_id != null ? '下载本集' : `下载选中分集（共 ${selectedEpisodeCount} 集）`) }}
          </button>
        </div>
      </div>
    </div>
  </section>
</template>
