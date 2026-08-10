<script lang="ts">
	import { cn } from '$lib/utils.js';
	import type { HTMLAttributes } from 'svelte/elements';

	let {
		ref = $bindable(null),
		class: className,
		value = 0,
		max = 100,
		...restProps
	}: HTMLAttributes<HTMLDivElement> & {
		value?: number;
		max?: number;
		ref?: HTMLDivElement | null;
	} = $props();

	const percentage = $derived(Math.min(Math.max((value / max) * 100, 0), 100));
</script>

<div
	bind:this={ref}
	class={cn('bg-primary/20 relative h-2 w-full overflow-hidden rounded-full', className)}
	{...restProps}
>
	<div
		class="bg-primary h-full w-full flex-1 transition-all"
		style="transform: translateX(-{100 - percentage}%)"
	></div>
</div>
