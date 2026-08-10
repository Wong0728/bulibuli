<script lang="ts">
	import { theme, toggleTheme } from '$lib/stores/theme';
	import { Button } from '$lib/components/ui/button/index.js';
	import SunIcon from '@lucide/svelte/icons/sun';
	import MoonIcon from '@lucide/svelte/icons/moon';

	function handleToggle() {
		toggleTheme();
	}
</script>

<Button
	variant="ghost"
	size="sm"
	class="h-8 w-8 p-0"
	title="切换主题 (当前: {$theme === 'light' ? '浅色' : $theme === 'dark' ? '深色' : '跟随系统'})"
	onclick={handleToggle}
>
	<SunIcon class="h-4 w-4 scale-100 rotate-0 transition-all dark:scale-0 dark:-rotate-90" />
	<MoonIcon
		class="absolute h-4 w-4 scale-0 rotate-90 transition-all dark:scale-100 dark:rotate-0"
	/>
	<span class="sr-only">切换主题</span>
</Button>
