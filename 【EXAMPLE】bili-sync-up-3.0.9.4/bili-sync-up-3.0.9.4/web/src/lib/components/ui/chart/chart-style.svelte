<script lang="ts">
	import { THEMES, type ChartConfig } from './chart-utils.js';
	import { onMount } from 'svelte';

	let { id, config }: { id: string; config: ChartConfig } = $props();

	const colorConfig = $derived(
		config ? Object.entries(config).filter(([, config]) => config.theme || config.color) : null
	);

	const themeContents = $derived.by(() => {
		if (!colorConfig || !colorConfig.length) return '';

		const themeContents = [];
		for (let [_theme, prefix] of Object.entries(THEMES)) {
			let content = `${prefix} [data-chart="${id}"] {\n`;
			const colors = colorConfig
				.map(([key, itemConfig]) => {
					const theme = _theme as keyof typeof itemConfig.theme;
					const color = itemConfig.theme?.[theme] || itemConfig.color;
					return color ? `\t--color-${key}: ${color};` : null;
				})
				.filter(Boolean);

			content += colors.join('\n') + '\n}';
			themeContents.push(content);
		}

		return themeContents.join('\n');
	});

	onMount(() => {
		if (themeContents) {
			const styleElement = document.createElement('style');
			styleElement.textContent = themeContents;
			styleElement.id = `chart-style-${id}`;
			document.head.appendChild(styleElement);

			return () => {
				const existingStyle = document.getElementById(`chart-style-${id}`);
				if (existingStyle) {
					document.head.removeChild(existingStyle);
				}
			};
		}
	});
</script>
