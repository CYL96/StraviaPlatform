<script lang="ts">
import type { ConsoleAssistantMessage } from '#lib/console-chat-types.js'
import * as m from '#lib/paraglide/messages.js'

let { message, active = false }: { message: ConsoleAssistantMessage; active?: boolean } = $props()
let now = $state(Date.now())
const seconds = $derived(
  Math.floor(Math.max(0, active ? now - Date.parse(message.createdAt) : (message.elapsedMs ?? 0)) / 1000),
)
const label = $derived(
  active
    ? m.console_chat_processed()
    : message.status === 'completed'
      ? m.console_chat_completed()
      : message.status === 'stopped'
        ? m.console_chat_cancelled()
        : message.status === 'incomplete'
          ? m.console_chat_incomplete()
          : m.console_chat_failed(),
)

$effect(() => {
  if (!active) return
  const timer = setInterval(() => (now = Date.now()), 1000)
  return () => clearInterval(timer)
})
</script>

<div class="flex items-baseline gap-1 text-xs text-muted-foreground">
  <span role="status">{label}</span>
  <span class="font-technical tabular-nums" role="timer" aria-live="off">{seconds}s</span>
</div>
