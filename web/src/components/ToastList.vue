<script setup lang="ts">
import { useToastStore } from '@/stores/toast';
const toast = useToastStore();
</script>

<template>
  <div class="toast-container" aria-live="polite" aria-atomic="false">
    <div v-for="t in toast.items" :key="t.id" :class="['toast', t.type]" :role="t.type === 'error' ? 'alert' : 'status'">
      <span>{{ t.message }}</span>
      <button type="button" class="toast-close" aria-label="关闭通知" @click="toast.dismiss(t.id)">×</button>
    </div>
  </div>
</template>
