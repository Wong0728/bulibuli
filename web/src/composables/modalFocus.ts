import { nextTick, onBeforeUnmount, watch, type Ref } from 'vue';

const FOCUSABLE = 'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** 模态栈：Esc 只关最顶层，避免一次按键把所有叠层模态同时关掉。 */
const modalStack: symbol[] = [];

/** 为 Vue 模态框提供 Esc 关闭、焦点初始定位、Tab 限制和关闭后焦点恢复。 */
export function useModalFocus(
  open: Ref<boolean>,
  root: Ref<HTMLElement | null>,
  onClose: () => void,
) {
  let restoreFocus: HTMLElement | null = null;
  const modalId = Symbol('modal');

  function focusables() {
    return Array.from(root.value?.querySelectorAll<HTMLElement>(FOCUSABLE) || [])
      .filter(element => !element.hidden && element.getClientRects().length > 0);
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      // 只有栈顶模态响应 Esc；下层模态保持打开。
      if (modalStack[modalStack.length - 1] !== modalId) return;
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
      last?.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first?.focus();
    }
  }

  watch(open, async value => {
    if (value) {
      restoreFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      await nextTick();
      focusables()[0]?.focus();
      modalStack.push(modalId);
      document.addEventListener('keydown', onKeydown);
    } else {
      document.removeEventListener('keydown', onKeydown);
      const index = modalStack.indexOf(modalId);
      if (index >= 0) modalStack.splice(index, 1);
      // 焦点只恢复给仍打开的下一层模态场景：若自己就是栈顶，恢复到打开前的元素。
      if (restoreFocus) restoreFocus.focus();
      restoreFocus = null;
    }
  }, { flush: 'post' });

  onBeforeUnmount(() => {
    document.removeEventListener('keydown', onKeydown);
    const index = modalStack.indexOf(modalId);
    if (index >= 0) modalStack.splice(index, 1);
    restoreFocus?.focus();
  });
}
