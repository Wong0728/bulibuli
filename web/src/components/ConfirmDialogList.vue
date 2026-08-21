<script setup lang="ts">
import { computed, ref } from 'vue';
import { confirmDialogs, resolveConfirm } from '@/composables/confirm';
import { useModalFocus } from '@/composables/modalFocus';

const root = ref<HTMLElement | null>(null);
const open = computed(() => confirmDialogs.value.length > 0);
const current = computed(() => confirmDialogs.value[0] || null);
function setRoot(element: unknown) { root.value = element as HTMLElement | null; }
useModalFocus(open, root, () => {
  const request = confirmDialogs.value[0];
  if (request) resolveConfirm(request, false);
});
</script>

<template>
  <div v-if="current" :ref="setRoot" id="confirm-modal" class="modal-overlay" role="dialog" aria-modal="true" aria-labelledby="confirm-modal-title" @click.self="resolveConfirm(current, false)">
    <div class="modal-container confirm-modal-container">
      <div class="modal-header">
        <i class="fa-solid fa-circle-question"></i>
        <span id="confirm-modal-title">{{ current.title }}</span>
      </div>
      <div class="confirm-modal-message">
        <div id="confirm-modal-message" style="white-space: pre-wrap; line-height: 1.6;">{{ current.message }}</div>
      </div>
      <div class="modal-footer">
        <button class="btn" id="confirm-modal-cancel" @click="resolveConfirm(current, false)">{{ current.cancelText || '取消' }}</button>
        <button :class="['btn', current.tone === 'danger' ? 'btn-danger' : 'btn-primary']" id="confirm-modal-ok" @click="resolveConfirm(current, true)">{{ current.confirmText || '确定' }}</button>
      </div>
    </div>
  </div>
</template>
