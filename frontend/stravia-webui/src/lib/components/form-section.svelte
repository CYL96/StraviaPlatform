<script lang="ts">
import type { Snippet } from 'svelte'

import { cn } from '#lib/utils.js'

interface Props {
  title: string
  description?: string
  /** 作用于整个小节的次要操作（例如移除整组配置），与标题同行靠右。 */
  actions?: Snippet
  children: Snippet
  class?: string
}

let { title, description, actions, children, class: className }: Props = $props()
const titleId = $props.id()
</script>

<!-- 长表单的小节：顶边线分隔，标题与说明在上，字段在下；字段自身按容器宽度决定左右或上下布局。 -->
<section
  class={cn('flex min-w-0 flex-col gap-4 border-t pt-5 first:border-t-0 first:pt-0', className)}
  aria-labelledby={titleId}>
  <div class="flex items-start justify-between gap-3">
    <div class="flex min-w-0 flex-col gap-1">
      <h3 id={titleId} class="text-sm font-semibold">{title}</h3>
      {#if description}
        <p class="max-w-prose text-[0.8125rem] leading-[1.45] text-pretty text-muted-foreground">{description}</p>
      {/if}
    </div>
    {#if actions}<div class="flex shrink-0 flex-wrap gap-2">{@render actions()}</div>{/if}
  </div>
  {@render children()}
</section>
