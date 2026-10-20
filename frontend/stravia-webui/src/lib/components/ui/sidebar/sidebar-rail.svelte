<script lang="ts">
import { cn, type WithElementRef } from '#lib/utils.js'
import { useSidebar } from './context.svelte.js'
import type { HTMLAttributes } from 'svelte/elements'

let {
  ref = $bindable(null),
  class: className,
  children,
  ...restProps
}: WithElementRef<HTMLAttributes<HTMLButtonElement>> = $props()

const sidebar = useSidebar()
</script>

<button
  bind:this={ref}
  data-sidebar="rail"
  data-slot="sidebar-rail"
  aria-label="Toggle Sidebar"
  tabindex={-1}
  onclick={sidebar.toggle}
  title="Toggle Sidebar"
  class={cn(
    'absolute inset-y-0 z-20 hidden w-0.5 bg-sidebar-border transition-colors ease-linear group-data-[side=left]:right-0 group-data-[side=right]:left-0 hover:bg-sidebar-foreground/50 sm:flex',
    'in-data-[side=left]:cursor-w-resize in-data-[side=right]:cursor-e-resize',
    '[[data-side=left][data-state=collapsed]_&]:cursor-e-resize [[data-side=right][data-state=collapsed]_&]:cursor-w-resize',
    '[[data-side=left][data-collapsible=offcanvas]_&]:translate-x-full [[data-side=right][data-collapsible=offcanvas]_&]:-translate-x-full',
    className,
  )}
  {...restProps}>
  {@render children?.()}
</button>
