<script lang="ts">
import * as m from '#lib/paraglide/messages.js'
import type { AllowanceSuspension } from '#lib/types/provider-allowance.js'
import { formatLogTime } from '#lib/format.js'
import { localeState } from '#lib/localization.svelte.js'
import StatusIndicator from '#lib/components/status-indicator.svelte'
import * as Tooltip from '#lib/components/ui/tooltip/index.js'
let { suspension }: { suspension: AllowanceSuspension } = $props()
const hintId = $props.id()
let open = $state(false)
</script>

<Tooltip.Root bind:open delayDuration={0} disableCloseOnTriggerClick>
  <Tooltip.Trigger
    type="button"
    class="relative inline-flex shrink-0 cursor-help items-center rounded-md text-start"
    aria-label={m.allowances_suspended()}
    aria-describedby={hintId}
    onclick={() => (open = true)}
    data-testid="allowance-suspension">
    <StatusIndicator compact label={m.allowances_suspended()} tone="warning" />
  </Tooltip.Trigger>
  <Tooltip.Content id={hintId} role="tooltip" sideOffset={4} class="max-w-[min(20rem,calc(100vw-2rem))]">
    <div class="grid gap-2">
      <p class="font-technical tabular-nums">
        {m.allowances_suspended_since({ time: formatLogTime(suspension.suspended_at, localeState.current) })}
      </p>
      <p class="break-words [overflow-wrap:anywhere]">
        {m.allowances_suspension_triggers({ keys: suspension.triggered_keys.join(', ') })}
      </p>
      {#if suspension.earliest_reset_at != null}
        <p class="font-technical tabular-nums">
          {m.allowances_reset_at({ time: formatLogTime(suspension.earliest_reset_at, localeState.current) })}
        </p>
      {/if}
    </div>
  </Tooltip.Content>
</Tooltip.Root>
