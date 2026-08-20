<script setup lang="ts">
import { ref, onMounted, computed } from 'vue';
import { useBloggerStore } from '@/stores/blogger';
import { useAuthStore } from '@/stores/auth';
import { useToastStore } from '@/stores/toast';
import { confirmDialog } from '@/composables/confirm';
import { blogger as bloggerApi } from '@/api';
import type { SearchBloggerResult, Blogger } from '@/api/types';

const blogger = useBloggerStore();
const auth = useAuthStore();
const toast = useToastStore();

const keyword = ref('');
const searchResults = ref<SearchBloggerResult[]>([]);
const searching = ref(false);

// 添加博主弹窗
const showAddModal = ref(false);
const addUid = ref<number | null>(null);
const addForm = ref({
  download_video: true,
  download_danmaku: false,
  download_comments: false,
  download_cover: true,
  burn_after_merge: false,
  filter_window_enabled: false,
});
const adding = ref(false);

onMounted(async () => {
  await blogger.refreshSaved().catch(() => {});
});

async function doSearch() {
  if (!keyword.value.trim()) return;
  searching.value = true;
  try {
    searchResults.value = await blogger.search(keyword.value);
  } catch (e: any) {
    toast.error(e?.message || '搜索失败');
    searchResults.value = [];
  } finally {
    searching.value = false;
  }
}

function openAddModal(uid: number) {
  addUid.value = uid;
  showAddModal.value = true;
  addForm.value = {
    download_video: true,
    download_danmaku: false,
    download_comments: false,
    download_cover: true,
    burn_after_merge: false,
    filter_window_enabled: false,
  };
}

function closeAddModal() {
  showAddModal.value = false;
  addUid.value = null;
}

async function confirmAdd() {
  if (!addUid.value) return;
  adding.value = true;
  try {
    await blogger.addBlogger(addUid.value, addForm.value);
    await blogger.refreshList().catch(() => {});
    toast.success('博主已添加');
    closeAddModal();
    await blogger.refreshSaved().catch(() => {});
  } catch (e: any) {
    toast.error(e?.message || '添加失败');
  } finally {
    adding.value = false;
  }
}

async function removeSaved(b: any) {
  // 后端 /api/blogger/saved/delete 用 { id: i32 }，老前端按 uid 删除会删错
  // （会去匹配 /api/blogger/delete 把整个自动任务干掉）。这里用 savedDelete。
  const id = (b && (b as any).id) ?? null;
  if (id == null) { toast.error('该博主无 id，请刷新后重试'); return; }
  try {
    await (bloggerApi as any).savedDelete(id);
    await blogger.refreshSaved();
    toast.success('已移除');
  } catch (e: any) {
    toast.error(e?.message || '操作失败');
  }
}

async function clearSaved() {
  if (!await confirmDialog({ title: '清空已添加博主', message: '确认清空所有已添加博主？', tone: 'danger' })) return;
  try {
    // 后端没提供"批量清空"，只能逐个删
    const ids = (blogger.savedBloggers || []).map((b: any) => b.id).filter((x: any) => x != null);
    for (const id of ids) {
      await (bloggerApi as any).savedDelete(id);
    }
    await blogger.refreshSaved();
    toast.success(`已清空 ${ids.length} 位`);
  } catch (e: any) {
    toast.error(e?.message || '操作失败');
  }
}

const noticeCount = computed(() => auth.noticeCount);
</script>

<template>
  <section class="tab-panel">
    <div class="card">
      <div class="card-title">
        <i class="fa-solid fa-user-plus"></i>
        <span>博主搜索</span>
        <button class="notice-dot-btn" v-if="noticeCount > 0" :title="`${noticeCount} 位博主改了头像或昵称`">
          <i class="fa-solid fa-bell"></i>
          <span class="notice-dot-badge">{{ noticeCount }}</span>
        </button>
      </div>
      <div class="blogger-search-bar">
        <label class="sr-only" for="blogger-search-input">博主名称或 UID</label>
        <input id="blogger-search-input" v-model="keyword" type="text" class="form-control"
               placeholder="输入博主名称或 UID，按回车搜索"
               @keydown.enter="doSearch" />
        <button class="btn btn-primary" :disabled="searching" @click="doSearch">
          <i class="fa-solid fa-search"></i> {{ searching ? '搜索中…' : '搜索' }}
        </button>
      </div>
      <div id="blogger-search-results" class="blogger-search-results">
        <div v-if="searchResults.length" class="list-grid">
          <div v-for="b in searchResults" :key="b.uid" class="list-item">
            <div class="list-item-avatar">
              <img v-if="b.face" :src="b.face" alt="avatar" />
              <div class="list-item-info">
                <div class="list-item-name">{{ b.name }}</div>
                <div class="list-item-uid">UID: {{ b.uid }}</div>
              </div>
            </div>
            <div class="list-item-sign">{{ b.sign || '暂无签名' }}</div>
            <button class="btn btn-sm btn-primary" @click="openAddModal(b.uid)">
              <i class="fa-solid fa-plus"></i> 添加监控
            </button>
          </div>
        </div>
        <div v-else class="empty-state blogger-search-empty-state">
          <i class="fa-solid fa-magnifying-glass"></i>
          <p>输入博主名称开始搜索</p>
        </div>
      </div>
    </div>

    <div class="card known-bloggers-card">
      <div class="card-title">
        <i class="fa-solid fa-bookmark"></i>
        <span>已添加博主（可在其他标签页下拉选择）</span>
        <button class="btn btn-sm btn-ghost known-bloggers-clear" @click="clearSaved">
          <i class="fa-solid fa-trash"></i> 清空
        </button>
      </div>
      <div v-if="blogger.savedBloggers.length === 0" class="empty-state" style="padding: 24px;">
        <p>暂无已添加博主</p>
      </div>
      <div v-else class="list-grid">
        <div v-for="b in blogger.savedBloggers" :key="b.uid" class="list-item">
          <div class="list-item-avatar">
            <img v-if="b.face" :src="b.face" />
            <div class="list-item-info">
              <div class="list-item-name">{{ b.name }}</div>
              <div class="list-item-uid">UID: {{ b.uid }}</div>
            </div>
          </div>
          <button class="btn btn-sm" @click="removeSaved(b)">
            <i class="fa-solid fa-trash"></i> 移除
          </button>
        </div>
      </div>
    </div>

    <!-- 添加博主模态框：与原版 blogger-modal 1:1 同构。 -->
    <div v-if="showAddModal" id="blogger-modal" class="modal-overlay" role="dialog" aria-modal="true" aria-labelledby="blogger-modal-title" @click.self="closeAddModal">
      <div class="modal-container modal-container-wide">
        <div class="modal-header">
          <i class="fa-solid fa-user-plus"></i>
          <span id="blogger-modal-title">添加监控博主</span>
          <button type="button" class="modal-close-btn" aria-label="关闭" @click="closeAddModal">
            <i class="fa-solid fa-times"></i>
          </button>
        </div>
        <div class="form-section">
          <div class="form-group form-full">
            <label><i class="fa-solid fa-id-card"></i> 博主 UID</label>
            <input type="text" class="form-control" :value="addUid ?? ''" readonly />
          </div>
          <div class="form-divider"><span>下载策略</span></div>
          <div class="form-group form-full policy-grid">
            <label class="choice-row">
              <span>下载视频</span>
              <span class="toggle-switch">
                <input type="checkbox" v-model="addForm.download_video" />
                <span class="slider"></span>
              </span>
            </label>
            <label class="choice-row">
              <span>下载弹幕</span>
              <span class="toggle-switch">
                <input type="checkbox" v-model="addForm.download_danmaku" />
                <span class="slider"></span>
              </span>
            </label>
            <label class="choice-row">
              <span>下载评论</span>
              <span class="toggle-switch">
                <input type="checkbox" v-model="addForm.download_comments" />
                <span class="slider"></span>
              </span>
            </label>
            <label class="choice-row">
              <span>下载封面</span>
              <span class="toggle-switch">
                <input type="checkbox" v-model="addForm.download_cover" />
                <span class="slider"></span>
              </span>
            </label>
            <label class="choice-row">
              <span>自动烧录弹幕</span>
              <span class="toggle-switch">
                <input type="checkbox" v-model="addForm.burn_after_merge" />
                <span class="slider"></span>
              </span>
            </label>
          </div>
        </div>
        <div class="modal-footer">
          <button class="btn" @click="closeAddModal">
            <i class="fa-solid fa-times"></i> 取消
          </button>
          <button class="btn btn-primary" :disabled="adding" @click="confirmAdd">
            <i class="fa-solid fa-check"></i> {{ adding ? '添加中…' : '确认添加' }}
          </button>
        </div>
      </div>
    </div>
  </section>
</template>
