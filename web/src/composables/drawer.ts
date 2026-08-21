/**
 * 视频详情抽屉的全局控制器：用 ref 暴露 show / data / open / close，
 * 让任意组件通过 useDrawer() 打开抽屉。
 *
 * 与老框架一致：抽屉 DOM 常驻，靠 .active class 驱动 CSS transition；
 * 打开时给 body 加 modal-open 锁定背景滚动。
 */
import { reactive } from 'vue';

export interface DrawerVideo {
  bvid: string;
  history_id?: number;
  title?: string;
  pic?: string;
  source?: 'manual' | 'history' | 'live';
  /** 手动查询来源：查询结果的原始视频条目（对应老框架 _state.manualQueryVideos[bvid]）。 */
  manualVideo?: Record<string, any>;
}

export const drawerState = reactive({
  visible: false,
  video: null as DrawerVideo | null,
});

export function openDrawer(video: DrawerVideo) {
  drawerState.video = video;
  drawerState.visible = true;
  document.body.classList.add('modal-open');
}

export function closeDrawer() {
  drawerState.visible = false;
  document.body.classList.remove('modal-open');
  // 保留 video 数据直到退场动画结束（节点常驻，仅移除 .active）。
}