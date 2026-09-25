/**
 * 选中房间实时信息（/api/live/room-info，切换时拉取）。
 * 从 TabLive.vue 抽出：generation 防竞态 + 立即首拉 + 手动强制刷新。
 */
import { ref, watch } from 'vue';
import { useLiveStore } from '@/stores/live';

export function useLiveRoomInfo(options: { onLoaded?: () => void } = {}) {
  const live = useLiveStore();

  const localRoomInfo = ref<any | null>(null);
  const localRoomInfoError = ref('');
  const localRoomInfoLoading = ref(false);
  let roomInfoGeneration = 0;

  watch(
    () => live.selectedSource?.room_id,
    async (roomId) => {
      const generation = ++roomInfoGeneration;
      if (!roomId) { localRoomInfo.value = null; return; }
      localRoomInfoLoading.value = true;
      localRoomInfoError.value = '';
      try {
        const r = await live.roomInfo(roomId);
        if (generation === roomInfoGeneration && live.selectedSource?.room_id === roomId) {
          localRoomInfo.value = r;
          localRoomInfoError.value = live.roomInfoError || '';
        }
        options.onLoaded?.();
      } finally {
        if (generation === roomInfoGeneration) localRoomInfoLoading.value = false;
      }
    },
    { immediate: true },
  );

  async function refreshRoomInfo() {
    if (!live.selectedSource) return;
    const roomId = live.selectedSource.room_id;
    const generation = ++roomInfoGeneration;
    localRoomInfoLoading.value = true;
    localRoomInfoError.value = '';
    try {
      const r = await live.roomInfo(roomId, { force: true });
      if (generation === roomInfoGeneration && live.selectedSource?.room_id === roomId) {
        localRoomInfo.value = r;
        localRoomInfoError.value = live.roomInfoError || '';
      }
    } finally {
      if (generation === roomInfoGeneration) localRoomInfoLoading.value = false;
    }
  }

  return { localRoomInfo, localRoomInfoError, localRoomInfoLoading, refreshRoomInfo };
}
