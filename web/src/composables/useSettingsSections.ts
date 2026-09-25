/**
 * 设置页分组与折叠状态（对齐老框架 settings.js loadSettingsFragment 的 groups +
 * bindCollapsibleSections）。从 TabSettings.vue 抽出的模块级单例：
 * 分组切换不持久化（无记忆），每次进入默认 basic；TabSettings 与各分组子组件共享。
 */
import { ref } from 'vue';

export type SettingsGroup = 'basic' | 'downloads' | 'advanced' | 'tools' | 'security';

/** 分组 → section 集合（对齐老框架 groups 常量）。 */
export const GROUP_SECTIONS: Record<SettingsGroup, string[]> = {
  basic: ['account', 'appearance', 'query', 'parallel', 'smart'],
  downloads: ['danmaku', 'aria2', 'ffmpeg', 'burn', 'subtitle', 'path', 'storage', 'retain', 'verify'],
  advanced: ['board', 'monitor', 'refresh', 'live-recording', 'update'],
  tools: ['logs'],
  security: ['local-config'],
};

export const GROUP_BUTTONS: Array<{ key: SettingsGroup; label: string }> = [
  { key: 'basic', label: '基础' },
  { key: 'downloads', label: '下载' },
  { key: 'advanced', label: '高级' },
  { key: 'tools', label: '工具' },
  { key: 'security', label: '安全' },
];

const group = ref<SettingsGroup>('basic');
const collapsedSections = ref(new Set<string>());

export function useSettingsSections() {
  function setGroup(g: SettingsGroup) { group.value = g; }

  function inGroup(section: string) {
    return (GROUP_SECTIONS[group.value] || GROUP_SECTIONS.basic).includes(section);
  }

  function toggleSection(section: string) {
    const next = new Set(collapsedSections.value);
    if (next.has(section)) next.delete(section);
    else next.add(section);
    collapsedSections.value = next;
  }

  /** 事件委托：容器级处理 header 点击 / Enter / Space 折叠。 */
  function onSectionsClick(e: Event) {
    const header = (e.target as HTMLElement).closest?.('.section-header') as HTMLElement | null;
    if (!header) return;
    const section = header.closest('.section-collapsible')?.getAttribute('data-section');
    if (section) toggleSection(section);
  }
  function onSectionsKeydown(e: KeyboardEvent) {
    if (e.key !== 'Enter' && e.key !== ' ') return;
    const header = (e.target as HTMLElement).closest?.('.section-header') as HTMLElement | null;
    if (!header) return;
    e.preventDefault();
    const section = header.closest('.section-collapsible')?.getAttribute('data-section');
    if (section) toggleSection(section);
  }

  return { group, collapsedSections, setGroup, inGroup, toggleSection, onSectionsClick, onSectionsKeydown };
}
