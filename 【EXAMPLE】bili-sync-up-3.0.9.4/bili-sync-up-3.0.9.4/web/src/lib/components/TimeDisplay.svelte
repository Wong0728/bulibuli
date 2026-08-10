<script lang="ts">
	import { formatTimestamp, getRelativeTime } from '$lib/utils/timezone';
	import { onMount } from 'svelte';

	export let timestamp: string | number | Date;
	export let format: 'datetime' | 'date' | 'time' | 'relative' = 'datetime';
	export let showTooltip: boolean = true;

	let formattedTime = '';
	let relativeTime = '';

	onMount(() => {
		updateTime();
	});

	function updateTime() {
		// 始终使用北京时间 (Asia/Shanghai)
		const beijingTimezone = 'Asia/Shanghai';
		if (format === 'relative') {
			formattedTime = getRelativeTime(timestamp, beijingTimezone);
			relativeTime = formatTimestamp(timestamp, beijingTimezone, 'datetime');
		} else {
			formattedTime = formatTimestamp(timestamp, beijingTimezone, format);
			relativeTime = getRelativeTime(timestamp, beijingTimezone);
		}
	}

	// 响应时间戳变化
	$: if (timestamp) {
		updateTime();
	}
</script>

{#if showTooltip && format === 'relative'}
	<span title={relativeTime} class="cursor-help">
		{formattedTime}
	</span>
{:else if showTooltip && format !== 'relative'}
	<span title={relativeTime} class="cursor-help">
		{formattedTime}
	</span>
{:else}
	<span>{formattedTime}</span>
{/if}
