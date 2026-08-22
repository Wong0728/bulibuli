<script setup lang="ts">
import { ref, computed, onActivated } from 'vue';
import { useBloggerStore } from '@/stores/blogger';
import { useToastStore } from '@/stores/toast';
import { confirmDialog } from '@/composables/confirm';
import { blogger as bloggerApi, video as videoApi } from '@/api';
import type { SearchBloggerResult, SavedBlogger } from '@/api/types';
import { useModalFocus } from '@/composables/modalFocus';

const blogger = useBloggerStore();
const toast = useToastStore();

const keyword = ref('');
const searchResults = ref<SearchBloggerResult[]>([]);
// 防抖锁：对齐老框架 _state.searchBloggersLock，请求期间不可重复发起。
const searching = ref(false);
// 区分初始空态（放大镜 + 提示）与搜索后空态（纯文案 + empty-state-padded）。
const searched = ref(false);
const searchMessage = ref('输入博主名称开始搜索');

// 已添加博主的提交锁（防重复 POST）。
const addingSavedUids = ref(new Set<number>());
// 博主变动通知弹窗。
const showNoticeModal = ref(false);
const noticeModalRoot = ref<HTMLElement | null>(null);

// 老框架 switchTab('search')：每次进入搜索页用 saved/list 检查资料变更黄点。
// （onActivated 在首次挂载时也会触发，等价老框架启动时的 saved/list 检查。）
onActivated(() => {
  void blogger.refreshSaved().catch(() => {});
});

/** 老框架 blogger-search.js formatFans：>=1万 显示 x.x 万。 */
function formatFans(n?: number | string) {
  const v = Number(n) || 0;
  if (v >= 10000) return `${(v / 10000).toFixed(1)}万`;
  return v.toString();
}

// --- 博主搜索（对齐 blogger-search.js searchBloggers 运行时行为） ---
async function doSearch() {
  if (searching.value) return;
  const query = keyword.value.trim();
  if (!query) {
    toast.error('请输入搜索关键字');
    return;
  }
  searching.value = true;
  searched.value = true;
  searchResults.value = [];
  const isAllDigits = /^\d+$/.test(query);
  let uidCard: SearchBloggerResult | null = null;

  // 纯数字输入：优先按 UID 精确查找；失败静默（console.warn），不连坐名称搜索。
  if (isAllDigits) {
    try {
      const exact = await bloggerApi.validateUid(query);
      if (exact?.exists) {
        uidCard = {
          uid: Number(exact.uid || query),
          name: exact.name || '',
          face: exact.face,
          sign: exact.sign,
          level: exact.level,
          fans: exact.fans,
          uid_exact: true,
        };
      }
    } catch (e) {
      console.warn('UID 精确查找失败:', e);
    }
  }

  // 名称搜索：对纯数字也搜索包含该数字的名称（UID 卡 + 名称结果双发展示）。
  try {
    const users = await blogger.search(query);
    if (!uidCard && users.length === 0) {
      searchMessage.value = '未找到匹配的博主';
    } else {
      searchResults.value = uidCard ? [uidCard, ...users] : users;
    }
  } catch {
    // 老框架 catch 分支：有 UID 卡只展示 UID 卡；否则固定文案
    // （网络失败由全局网络反馈链提示，本地不重复 toast）。
    if (uidCard) {
      searchResults.value = [uidCard];
    } else {
      searchMessage.value = '搜索请求失败';
    }
  } finally {
    searching.value = false;
  }
}

// --- 已添加博主管理（对齐 addBloggerToKnown / removeKnownBlogger / clearKnownBloggers） ---
async function addSaved(b: SearchBloggerResult) {
  if (addingSavedUids.value.has(b.uid)) return;
  addingSavedUids.value.add(b.uid);
  try {
    // 老框架点击时先拉最新已添加列表查重。
    await blogger.refreshSaved().catch(() => {});
    const existing = blogger.savedBloggers.find(item => String(item.uid) === String(b.uid));
    if (existing) {
      toast.info(`博主 ${existing.name || b.uid} 已在列表中`);
      return;
    }
    // 这里只收藏搜索结果，不创建自动任务。
    await bloggerApi.savedAdd(b.uid, {
      name: b.name || '',
      face: b.face || '',
      sign: b.sign || '',
      level: b.level || 0,
      fans: b.fans || 0,
    });
    await blogger.refreshSaved();
    toast.success(`已添加博主 ${b.name || b.uid}`);
  } catch (e: any) {
    // 老框架：网络失败交给全局网络链，其余展示后端 message。
    if (e?.code !== 0) toast.error(e?.message || '添加博主失败');
  } finally {
    addingSavedUids.value.delete(b.uid);
    addingSavedUids.value = new Set(addingSavedUids.value);
  }
}

async function removeSaved(b: SavedBlogger) {
  // 后端 /api/blogger/saved/delete 用 { id }（按 uid 删会误删自动任务）；老框架不带版本号。
  const id = b?.id ?? null;
  if (id == null) return;
  try {
    await bloggerApi.savedDelete(id);
    await blogger.refreshSaved();
  } catch (e: any) {
    // 失败必须可见：点击像没反应会让用户以为已移除。
    if (e?.code === 0) toast.error('网络连接异常，移除博主失败');
    else toast.error(e?.message || '移除博主失败');
    console.warn('移除已添加博主失败:', e);
  }
}

async function clearSaved() {
  if (!await confirmDialog({
    title: '清空列表',
    message: '确定要清空已添加博主列表吗？已有自动任务不会受到影响。',
    confirmText: '清空',
    tone: 'danger',
  })) return;
  try {
    // 老框架按列表顺序逐个删除，不做并发。
    for (const item of [...blogger.savedBloggers]) {
      if (item.id == null) continue;
      await bloggerApi.savedDelete(item.id);
    }
    await blogger.refreshSaved();
    toast.success('已清空已添加博主');
  } catch (e: any) {
    // 老框架：网络失败交给全局网络链，其余固定文案。
    if (e?.code !== 0) toast.error('清空失败');
  }
}

// --- 博主资料变更通知（黄点 + 弹窗，数据源 saved/list，对齐 checkBloggerProfileNotices） ---
const noticeBloggers = computed(() => blogger.savedBloggers.filter(b => b.notice_visible));
const noticeCount = computed(() => noticeBloggers.value.length);

function closeNoticeModal() { showNoticeModal.value = false; }
useModalFocus(showNoticeModal, noticeModalRoot, closeNoticeModal);

/** 变更标签：改名/改头像/资料变更（对齐 showBloggerNoticeModal）。 */
function noticeChangeLabel(b: SavedBlogger) {
  const nameChanged = b.last_seen_name && b.last_seen_name !== b.name;
  const faceChanged = b.last_seen_face && b.last_seen_face !== b.face;
  const changes: string[] = [];
  if (nameChanged) changes.push('改名');
  if (faceChanged) changes.push('改头像');
  return changes.length > 0 ? changes.join('、') : '资料变更';
}

/** 变更时间：last_seen_at 本地化展示（对齐 new Date(...).toLocaleString()）。 */
function noticeTime(b: SavedBlogger) {
  return b.last_seen_at ? new Date(b.last_seen_at).toLocaleString() : '';
}

async function acknowledgeNotice(uid: number | string) {
  try {
    await bloggerApi.acknowledgeChange(uid);
    // 重新拉 saved/list 刷新黄点与弹窗列表；确认完最后一条自动关闭。
    await blogger.refreshSaved().catch(() => {});
    if (noticeBloggers.value.length === 0) closeNoticeModal();
    toast.success('已确认');
  } catch {
    // 老框架 catch：固定文案，不读后端 message。
    toast.error('确认失败');
  }
}

async function acknowledgeAllNotices() {
  const list = noticeBloggers.value;
  if (list.length === 0) { closeNoticeModal(); return; }
  try {
    // 后端 acknowledge-batch 返回 { affected }；老框架 toast 误读 acknowledged（恒 0），
    // 按“显示确认条数”的意图读 affected。
    const r = await bloggerApi.acknowledgeBatch(list.map(b => b.uid));
    await blogger.refreshSaved().catch(() => {});
    closeNoticeModal();
    toast.success(`已确认 ${r?.affected || 0} 条`);
  } catch (e: any) {
    toast.error(`批量确认失败: ${e?.message || ''}`);
  }
}

function imageUrl(url?: string) { return url ? videoApi.proxyImage(url) : ''; }

/** 对齐老框架 data-image-error="hide"：加载失败仅隐藏图片，不显示替代占位。 */
function imageError(event: Event) {
  (event.target as HTMLImageElement).hidden = true;
}
</script>

<template>
  <section class="tab-panel">
    <div class="card">
      <div class="card-title">
        <i class="fa-solid fa-user-plus"></i>
        <span>博主搜索</span>
        <button id="blogger-notice-dot" class="notice-dot-btn" v-if="noticeCount > 0" title="有博主改了头像或昵称" @click="showNoticeModal = true">
          <i class="fa-solid fa-bell"></i>
          <span id="blogger-notice-count" class="notice-dot-badge">{{ noticeCount }}</span>
        </button>
      </div>
      <div class="blogger-search-bar">
        <label class="sr-only" for="blogger-search-input">博主名称或 UID</label>
        <input id="blogger-search-input" v-model="keyword" type="text" class="form-control"
               placeholder="输入博主名称或 UID，按回车搜索"
               @keydown.enter="doSearch" />
        <button id="blogger-search-btn" class="btn btn-primary" data-network-required="true" :disabled="searching" @click="doSearch">
          <template v-if="searching"><span class="loading"></span> 搜索中</template>
          <template v-else><i class="fa-solid fa-search"></i> 搜索</template>
        </button>
      </div>
      <div id="blogger-search-results" class="blogger-search-results">
        <div v-if="searching" class="loading search-loading"></div>
        <template v-else-if="searchResults.length">
          <div v-for="b in searchResults" :key="b.uid" :class="['blogger-search-card', { 'uid-exact-match': b.uid_exact }]">
            <img v-if="b.face" :src="imageUrl(b.face)" class="blogger-avatar" alt="" @error="imageError" />
            <div v-else class="blogger-avatar-placeholder"><i class="fa-solid fa-user"></i></div>
            <div class="blogger-info">
              <div class="blogger-name">{{ b.name }} <span v-if="b.uid_exact" class="uid-match-badge">UID 精确匹配</span></div>
              <div class="blogger-meta">UID: {{ b.uid }} · Lv{{ b.level || 0 }} · 粉丝 {{ formatFans(b.fans) }}<template v-if="b.videos_count != null"> · 视频 {{ b.videos_count }}</template></div>
              <div class="blogger-sign">{{ b.sign || '' }}</div>
            </div>
            <div class="blogger-search-actions">
              <button class="btn btn-sm btn-primary" data-mutating :disabled="addingSavedUids.has(b.uid)" @click="addSaved(b)">
                <i class="fa-solid fa-plus"></i> 添加
              </button>
            </div>
          </div>
        </template>
        <div v-else :class="['empty-state', 'blogger-search-empty-state', { 'empty-state-padded': searched }]">
          <i v-if="!searched" class="fa-solid fa-magnifying-glass"></i>
          <p>{{ searchMessage }}</p>
        </div>
      </div>
    </div>

    <div class="card known-bloggers-card">
      <div class="card-title">
        <i class="fa-solid fa-bookmark"></i>
        <span>已添加博主（可在其他标签页下拉选择）</span>
        <button id="clear-known-bloggers-btn" class="btn btn-sm btn-ghost known-bloggers-clear" data-mutating @click="clearSaved">
          <i class="fa-solid fa-trash"></i> 清空
        </button>
      </div>
      <div v-if="blogger.savedBloggers.length === 0" id="known-blogger-list">
        <div class="empty-state empty-state-padded">
          <p>暂无已添加博主</p>
          <p class="empty-hint">使用上方搜索添加博主</p>
        </div>
      </div>
      <div v-else id="known-blogger-list">
        <div v-for="b in blogger.savedBloggers" :key="b.uid" class="known-blogger-card">
          <img v-if="b.face" :src="imageUrl(b.face)" class="blogger-avatar" alt="" @error="imageError" />
          <div v-else class="blogger-avatar blogger-avatar-placeholder"><i class="fa-solid fa-user"></i></div>
          <div class="blogger-info">
            <div class="blogger-name">{{ b.name }}</div>
            <div class="blogger-meta">UID: {{ b.uid }} · Lv{{ b.level || 0 }} · 粉丝 {{ formatFans(b.fans) }}</div>
          </div>
          <button class="btn btn-sm btn-danger" data-mutating @click="removeSaved(b)">
            <i class="fa-solid fa-times"></i>
          </button>
        </div>
      </div>
    </div>

    <!-- 博主资料变更通知弹窗（DOM 对齐老框架 index.html + showBloggerNoticeModal：旧→新） -->
    <div v-if="showNoticeModal" ref="noticeModalRoot" id="blogger-notice-modal" class="modal-overlay" role="dialog" aria-modal="true" aria-labelledby="blogger-notice-title" @click.self="closeNoticeModal">
      <div class="modal-container">
        <div class="modal-header">
          <i class="fa-solid fa-bell"></i>
          <span id="blogger-notice-title">博主资料变更</span>
        </div>
        <div class="form-section">
          <div id="blogger-notice-list">
            <div v-if="noticeBloggers.length === 0" class="empty-state notice-empty">
              <p>暂无资料变更通知</p>
            </div>
            <div v-for="b in noticeBloggers" :key="b.uid" class="blogger-notice-item">
              <div class="blogger-notice-header">
                <span class="blogger-notice-name">{{ b.name || b.uid }}</span>
                <span class="blogger-notice-tag">{{ noticeChangeLabel(b) }}</span>
                <span class="blogger-notice-time">{{ noticeTime(b) }}</span>
              </div>
              <div class="blogger-notice-compare">
                <div class="blogger-notice-col">
                  <div class="blogger-notice-label">旧</div>
                  <!-- 只改头像（或只改名）时 last_seen_name/face 为空：
                       旧栏回退显示当前值，避免"原来没有名字"的错觉。 -->
                  <img v-if="b.last_seen_face || b.face" :src="imageUrl(b.last_seen_face || b.face)" class="blogger-avatar-sm" alt="" @error="imageError" />
                  <div v-else class="blogger-avatar-sm blogger-avatar-placeholder"><i class="fa-solid fa-user"></i></div>
                  <div class="blogger-notice-name-old">{{ b.last_seen_name || b.name || '--' }}</div>
                </div>
                <div class="blogger-notice-arrow"><i class="fa-solid fa-arrow-right"></i></div>
                <div class="blogger-notice-col">
                  <div class="blogger-notice-label">新</div>
                  <img v-if="b.face" :src="imageUrl(b.face)" class="blogger-avatar-sm" alt="" @error="imageError" />
                  <div v-else class="blogger-avatar-sm blogger-avatar-placeholder"><i class="fa-solid fa-user"></i></div>
                  <div class="blogger-notice-name-new">{{ b.name || '--' }}</div>
                </div>
              </div>
              <button class="btn btn-sm btn-ghost" data-mutating @click="acknowledgeNotice(b.uid)">
                <i class="fa-solid fa-check"></i> 知道了
              </button>
            </div>
          </div>
        </div>
        <div class="modal-footer">
          <button class="btn" @click="closeNoticeModal">
            <i class="fa-solid fa-times"></i> 关闭
          </button>
          <button class="btn btn-primary" data-mutating @click="acknowledgeAllNotices">
            <i class="fa-solid fa-check"></i> 全部知道了
          </button>
        </div>
      </div>
    </div>
  </section>
</template>
