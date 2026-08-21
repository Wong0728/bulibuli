import { nextTick, onBeforeUnmount, watch, type Ref } from 'vue';

const FOCUSABLE = 'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** 为 Vue 模态框提供 Esc 关闭、焦点初始定位、Tab 限制和关闭后焦点恢复。 */
export function useModalFocus(
  open: Ref<boolean>,
  root: Ref<HTMLElement | null>,
  onClose: () => void,
) {
  let restoreFocus: HTMLElement | null = null;

  function focusables() {
    return Array.from(root.value?.querySelectorAll<HTMLElement>(FOCUSABLE) || [])
      .filter(element => !element.hidden && element.getClientRects().length > 0);
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      event.preventDefault();
      onClose();
      return;
    }
    if (event.key !== 'Tab') return;
    const elements = focusables();
    if (!elements.length) return;
    const first = elements[0];
    const last = elements[elements.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  }

  watch(open, async value => {
    if (value) {
      restoreFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      await nextTick();
      focusables()[0]?.focus();
      document.addEventListener('keydown', onKeydown);
    } else {
      document.removeEventListener('keydown', onKeydown);
      restoreFocus?.focus();
      restoreFocus = null;
    }
  }, { flush: 'post' });

  onBeforeUnmount(() => {
    document.removeEventListener('keydown', onKeydown);
    restoreFocus?.focus();
  });
}
