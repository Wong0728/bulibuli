<script setup lang="ts">
import { confirmDialogs, resolveConfirm } from '@/composables/confirm';
</script>

<template>
  <div v-for="(d, i) in confirmDialogs" :key="i" id="confirm-modal" class="modal-overlay" role="dialog" aria-modal="true" aria-labelledby="confirm-modal-title" @click.self="resolveConfirm(d, false)">
    <div class="modal-container confirm-modal-container">
      <div class="modal-header">
        <i class="fa-solid fa-circle-question"></i>
        <span id="confirm-modal-title">{{ d.title }}</span>
      </div>
      <div class="confirm-modal-message">
        <div id="confirm-modal-message" style="white-space: pre-wrap; line-height: 1.6;">{{ d.message }}</div>
      </div>
      <div class="modal-footer">
        <button class="btn" id="confirm-modal-cancel" @click="resolveConfirm(d, false)">{{ d.cancelText || '取消' }}</button>
        <button :class="['btn', d.tone === 'danger' ? 'btn-danger' : 'btn-primary']" id="confirm-modal-ok" @click="resolveConfirm(d, true)">{{ d.confirmText || '确定' }}</button>
      </div>
    </div>
  </div>
</template>
