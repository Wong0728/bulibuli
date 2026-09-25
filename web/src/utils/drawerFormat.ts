/**
 * 视频详情抽屉的纯格式化工具与常量（与老框架 utils.js / history.js 对齐）。
 * 从 VideoDrawer.vue 抽出：无副作用、无状态，供抽屉主组件与子组件共用。
 */

/** 与老框架 drawer.js::_ALL_QUALITY_OPTIONS 一致；不含 126/127（后端下载白名单最高 125）。 */
export interface QualityOption { qn: number; label: string; tag: string }

export const QUALITY_OPTIONS: QualityOption[] = [
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

export const TYPE_LABELS: Record<string, string> = {
  video: '视频', danmaku_video: '弹幕版视频', audio: '音频', cover: '封面',
  danmaku: '弹幕', subtitle: 'CC 字幕', comment: '评论', other: '文件',
};

export const TYPE_ICONS: Record<string, string> = {
  video: 'fa-film', danmaku_video: 'fa-fire', audio: 'fa-music', cover: 'fa-image',
  danmaku: 'fa-comment-dots', subtitle: 'fa-closed-captioning', comment: 'fa-comments', other: 'fa-file',
};

/** 对齐老框架 history.js::renderSidecarIcons 四项（视频/弹幕/评论/字幕，✓/— 形态）。 */
export const SIDECAR_ICONS = [
  { key: 'video', label: '视频', icon: 'fa-film' },
  { key: 'danmaku', label: '弹幕', icon: 'fa-comment-dots' },
  { key: 'comments', label: '评论', icon: 'fa-comments' },
  { key: 'subtitle', label: '字幕', icon: 'fa-closed-captioning' },
] as const;

export function clampPercent(value: any): number {
  const n = Number(value);
  return Number.isFinite(n) ? Math.min(100, Math.max(0, n)) : 0;
}

export function formatSize(bytes?: number): string {
  // 对齐老框架 utils.js::formatFileSize：log 取单位 + toFixed(2) 后 parseFloat 去尾零（1.75 MB 而非 1.8 MB）。
  const value = Number(bytes);
  if (!Number.isFinite(value) || value <= 0) return '0 B';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  const index = Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1);
  return `${parseFloat((value / Math.pow(1024, index)).toFixed(2))} ${units[index]}`;
}

export function formatSpeed(b?: number): string { return b ? `${formatSize(b)}/s` : ''; }

export function formatDuration(s?: number): string {
  if (!s) return '';
  const sec = Math.floor(s % 60), min = Math.floor((s / 60) % 60), hr = Math.floor(s / 3600);
  const mm = String(min).padStart(2, '0'), ss = String(sec).padStart(2, '0');
  return hr > 0 ? `${hr}:${mm}:${ss}` : `${min}:${ss}`;
}

export function formatViewCount(view?: number): string {
  const v = Number(view) || 0;
  if (v >= 100000000) return (v / 100000000).toFixed(1) + '亿';
  if (v >= 10000) return (v / 10000).toFixed(1) + '万';
  return String(v);
}

export function formatTimestamp(ts?: number): string {
  const t = Number(ts) || 0;
  if (t <= 0) return '';
  const d = new Date(t * 1000);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function locationLabel(location?: string): string {
  const loc = location || 'other';
  if (loc === 'manual') return '手动下载区域';
  if (loc.startsWith('auto:')) return `自动下载区域 · UID ${loc.slice(5)}`;
  if (loc.startsWith('other:')) return `自定义目录 · ${loc.slice(6)}`;
  return '其他下载区域';
}

export function formatArchiveVersion(version?: string): string {
  if (!version) return '最新';
  const m = /^(\d{4})(\d{2})(\d{2})-(\d{2})(\d{2})(\d{2})-(\d{3})$/.exec(version);
  if (!m) return version;
  return `${m[1]}-${m[2]}-${m[3]} ${m[4]}:${m[5]}:${m[6]}.${m[7]}`;
}

export function sidecarVersionLabel(f: any): string {
  return f.version ? formatArchiveVersion(f.version) : (f.is_current ? '当前最新版' : '最新副本');
}

export function fileVersionLabel(f: any): string {
  return f.version ? formatArchiveVersion(f.version) : (f.is_current ? '当前' : '副本');
}

export function hasSidecar(value: any): boolean {
  // 对齐老框架 renderSidecarIcons 的 `sidecar[it.key]` 纯真值判断（后端字段是 bool）。
  return !!value;
}

export function formatDanmakuTime(item: any): string {
  const raw = Number(item?.progress ?? item?.time ?? item?.ctime ?? 0);
  const seconds = raw > 10000 ? raw / 1000 : raw;
  return `${Math.max(0, Math.floor(seconds / 60))}:${String(Math.max(0, Math.floor(seconds % 60))).padStart(2, '0')}`;
}

export function formatSidecarTime(ts?: number): string {
  if (!ts) return '';
  const d = new Date(ts * 1000);
  return Number.isNaN(d.getTime()) ? '' : d.toLocaleString();
}

export function commentUserTone(value?: string): string {
  const tones: Record<string, string> = {
    '#00a1d6': ' cmt-user-cyan', '#00aeec': ' cmt-user-cyan', '#67c23a': ' cmt-user-green',
    '#9b59b6': ' cmt-user-purple', '#f39800': ' cmt-user-orange', '#fb7299': ' cmt-user-pink', '#ff6699': ' cmt-user-pink',
  };
  return tones[String(value || '').trim().toLowerCase()] || '';
}

/** 手动抽屉「保存到本机」：挑选最接近首选清晰度的可用流（DASH 分离流）。 */
export function pickVideoQuality(qualities: any[], acceptQuality: Set<number>, preferredQn: number): any | null {
  const available = qualities.filter(q => acceptQuality.has(q.quality) && q.url);
  if (!available.length) return null;
  return available.find(q => q.quality === preferredQn)
    || available.filter(q => q.quality <= preferredQn).sort((a, b) => b.quality - a.quality)[0]
    || [...available].sort((a, b) => b.quality - a.quality)[0];
}

export function formatAudioQuality(id: number, kbps: number): string {
  const names: Record<number, string> = { 30251: 'Hi-Res 无损', 30250: '杜比全景声', 30280: '192K', 30232: '132K', 30216: '64K' };
  if (names[id]) return names[id];
  return kbps > 0 ? `${kbps}K` : '未知音质';
}
