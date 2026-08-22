import { createApp } from 'vue';
import { createPinia, type Pinia } from 'pinia';
import App from './App.vue';
import './styles/index.css';
import { useToastStore } from './stores/toast';

const app = createApp(App);
const pinia = createPinia();
app.use(pinia);
app.mount('#app');

// --- 全局错误兜底 ---
// errorHandler 在 app 创建后注册即可生效；toast store 依赖 pinia，
// 这里持有 pinia 实例引用（而非依赖"激活的 pinia"），保证任何时机都能安全取用。
let lastCrashToastAt = 0;

/** 向用户展示兜底错误提示（3 秒节流，避免批量异常时 toast 刷屏）。 */
function showCrashToast(message: string) {
  const now = Date.now();
  if (now - lastCrashToastAt < 3000) return;
  lastCrashToastAt = now;
  try {
    useToastStore(pinia).error(message);
  } catch (toastError) {
    console.error('兜底错误提示展示失败:', toastError);
  }
}

/** 组件内未捕获异常（渲染 / 侦听器 / 生命周期钩子）的统一入口。 */
app.config.errorHandler = (err, _instance, info) => {
  console.error(`[全局异常] ${info}:`, err);
  showCrashToast('界面出现异常，请刷新页面重试');
};

/** store/action 之外漏网的 Promise 拒绝（对齐 Vue 体系外的最后一道防线）。 */
window.addEventListener('unhandledrejection', (event) => {
  console.error('[未处理的 Promise 异常]:', event.reason);
  showCrashToast('操作出现异常，请刷新页面重试');
});
