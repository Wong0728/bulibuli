import { video as videoApi } from '@/api';

/**
 * 头像/封面统一走后端图片代理：B 站对图片做了 Referer 防盗链，
 * 浏览器直链会 403（协议相对地址 `//i0.hdslb.com/...` 由后端
 * `bili_url_policy::normalize_syntax` 补 `https:`，前端不必再归一）。
 */
export function imageUrl(url?: string) {
  return url ? videoApi.proxyImage(url) : '';
}

/** 加载失败：隐藏图片并显示紧随其后的占位兄弟节点（多数卡片带 placeholder）。 */
export function imageError(event: Event) {
  const image = event.target as HTMLImageElement;
  image.hidden = true;
  image.nextElementSibling?.removeAttribute('hidden');
}

/** 加载失败：仅隐藏图片，不显示占位（对齐老框架 data-image-error="hide"）。 */
export function hideImageOnError(event: Event) {
  (event.target as HTMLImageElement).hidden = true;
}
