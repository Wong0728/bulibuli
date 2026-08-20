/**
 * 视频详情抽屉的全局控制器：用 ref 暴露 show / data / open / close，
 * 让任意组件通过 useDrawer() 打开抽屉。
 */
import { reactive, ref } from 'vue';

export interface DrawerVideo {
  bvid: string;
  history_id?: number;
  title?: string;
  pic?: string;
  source?: 'manual' | 'history' | 'live';
}

export const drawerState = reactive({
  visible: false,
  video: null as DrawerVideo | null,
});

export function openDrawer(video: DrawerVideo) {
  drawerState.video = video;
  drawerState.visible = true;
}

export function closeDrawer() {
  drawerState.visible = false;
  drawerState.video = null;
}