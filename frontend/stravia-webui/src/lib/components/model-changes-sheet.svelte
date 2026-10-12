<script lang="ts">
import * as m from '$lib/paraglide/messages.js'
import { createInfiniteQuery, createQuery } from '@tanstack/svelte-query'
import { tick } from 'svelte'
import { SvelteMap } from 'svelte/reactivity'
import { admin } from '$lib/admin-client'
import { localizeBackendErrorMessage } from '$lib/backend-error'
import { formatLogTime } from '$lib/format'
import {
  changeTypes,
  changeLabel,
  sourceLabel,
  policyLabel,
  specificationField,
  specificationValue,
  modelChangeReason,
} from '$lib/model-changes'
import type { ModelChangeType, ModelChangeSource, ModelChangeRecord } from '$lib/types'
import * as Sheet from '$lib/components/ui/sheet'
import * as Field from '$lib/components/ui/field'
import * as Select from '$lib/components/ui/select'
import * as Empty from '$lib/components/ui/empty'
import { Input } from '$lib/components/ui/input'
import { Button } from '$lib/components/ui/button'
import { Badge } from '$lib/components/ui/badge'
import { Spinner } from '$lib/components/ui/spinner'
import RequestFailure from '$lib/components/request-failure.svelte'
let open = $state(false)
let providerId = $state('')
let type = $state<ModelChangeType | ''>('')
let source = $state<ModelChangeSource | ''>('')
let search = $state('')
let selectedId = $state('')
let expanded = $state<Record<string, boolean>>({})
let restoreTarget: HTMLElement | null = null
let selectedElement = $state<HTMLDivElement>()
const providers = createQuery(() => ({ queryKey: ['providers'], queryFn: admin.providers.list, enabled: open }))
const history = createInfiniteQuery(() => ({
  queryKey: ['model-changes', providerId, type, source, search],
  enabled: open,
  refetchInterval: 15_000,
  initialPageParam: undefined as string | undefined,
  queryFn: ({ pageParam }) =>
    admin.modelChanges.list({
      provider_id: providerId || undefined,
      type: type || undefined,
      source: source || undefined,
      q: search || undefined,
      cursor: pageParam,
      limit: 25,
    }),
  getNextPageParam: (last) => last.next_cursor ?? undefined,
}))
const selected = createQuery(() => ({
  queryKey: ['model-change', selectedId],
  queryFn: () => admin.modelChanges.get(selectedId),
  enabled: open && Boolean(selectedId),
}))
const records = $derived(history.data?.pages.flatMap((item) => item.records) ?? [])
const serviceOptions = $derived.by(() => {
  const options = new SvelteMap((providers.data ?? []).map((provider) => [provider.id, provider.name]))
  for (const record of records) {
    if (!options.has(record.provider_id))
      options.set(
        record.provider_id,
        `${record.provider_name}${record.provider_deleted ? ` · ${m.model_changes_deleted()}` : ''}`,
      )
  }
  if (selected.data && !options.has(selected.data.provider_id))
    options.set(
      selected.data.provider_id,
      `${selected.data.provider_name}${selected.data.provider_deleted ? ` · ${m.model_changes_deleted()}` : ''}`,
    )
  return options
})
const filtered = $derived(Boolean(providerId || type || source || search))
function focusFallback(): HTMLElement | null {
  for (const selector of [
    '[data-model-changes-trigger]',
    'main [aria-current="page"]',
    'nav a[aria-current="page"]',
    'header button[aria-expanded]',
  ]) {
    const target = [...document.querySelectorAll<HTMLElement>(selector)].find(
      (element) => element.getClientRects().length > 0,
    )
    if (target) return target
  }
  return null
}
export function show(defaultProvider = ''): void {
  restoreTarget =
    document.activeElement instanceof HTMLElement && document.activeElement !== document.body
      ? document.activeElement
      : focusFallback()
  providerId = defaultProvider
  type = ''
  source = ''
  search = ''
  selectedId = ''
  expanded = {}
  open = true
}
export function reveal(id: string): void {
  const active = document.activeElement
  restoreTarget =
    active instanceof HTMLElement && active !== document.body && !active.closest('[data-sonner-toast]')
      ? active
      : focusFallback()
  selectedId = id
  expanded = { ...expanded, [id]: true }
  open = true
}
$effect(() => {
  if (open && selected.data && selectedElement) {
    const target = selectedElement
    void tick().then(() => {
      target.scrollIntoView({ block: 'nearest' })
      target.focus()
    })
  }
})
</script>

{#snippet batch(record: ModelChangeRecord)}
  <article class="min-w-0 border-b py-4">
    <Button
      variant="ghost"
      class="h-auto min-h-10 w-full justify-between whitespace-normal text-left"
      aria-expanded={Boolean(expanded[record.id])}
      aria-controls={`batch-${record.id}`}
      onclick={() => (expanded[record.id] = !expanded[record.id])}>
      <span class="min-w-0 break-words"
        >{record.provider_name}{#if record.provider_deleted}
          · {m.model_changes_deleted()}{/if}{#if record.initial_sync}
          · {m.model_changes_initial()}{/if}<span class="block"
          >{record.outcome === 'failure'
            ? m.model_changes_failed()
            : record.outcome === 'partial_success'
              ? m.model_changes_partial()
              : [...new Set(record.changes.flatMap((item) => item.types))]
                  .map(
                    (kind) =>
                      `${changeLabel(kind)} ${record.changes.filter((item) => item.types.includes(kind)).length}`,
                  )
                  .join(' · ')}</span
        ></span>
      <span aria-hidden="true">{expanded[record.id] ? '−' : '+'}</span>
    </Button>
    <p class="mt-1 px-3 text-xs text-muted-foreground">
      <time datetime={record.checked_at}>{formatLogTime(record.checked_at)}</time> · {sourceLabel(record.source)}
    </p>
    {#if expanded[record.id]}
      <div id={`batch-${record.id}`} class="flex min-w-0 flex-col gap-4 px-3 pt-4">
        {#if record.error}<p class="break-words text-destructive">{modelChangeReason(record.error)}</p>{/if}
        {#each record.changes as item (item.model_id)}
          <div class="flex min-w-0 flex-col gap-2">
            <p class="break-all font-medium">{item.name || item.model_id}</p>
            {#if item.name && item.name !== item.model_id}<p class="break-all font-technical">{item.model_id}</p>{/if}
            <div class="flex flex-wrap gap-2">
              {#each item.types as kind (kind)}<Badge variant="outline" class="h-auto text-sm"
                  >{changeLabel(kind)}</Badge
                >{/each}
            </div>
            {#if item.types.includes('selection_policy_changed') && item.before_policy && item.after_policy}<p>
                {policyLabel(item.before_policy)} → {policyLabel(item.after_policy)}
              </p>{/if}
            {#each item.specification as diff (diff.field)}
              <dl class="flex min-w-0 flex-col gap-1">
                <dt class="font-medium">{specificationField(diff.field)}</dt>
                <dd class="break-words">{m.model_changes_before()}: {specificationValue(diff.before)}</dd>
                <dd class="break-words">{m.model_changes_after()}: {specificationValue(diff.after)}</dd>
              </dl>
            {/each}
          </div>
        {/each}
      </div>
    {/if}
  </article>
{/snippet}
<Sheet.Root bind:open>
  <Sheet.Content
    side="right"
    class="w-full! max-w-[32rem]!"
    closeLabel={m.common_close()}
    aria-describedby={undefined}
    onCloseAutoFocus={(event: Event) => {
      const target =
        restoreTarget?.isConnected && restoreTarget.getClientRects().length > 0 ? restoreTarget : focusFallback()
      if (target) {
        event.preventDefault()
        target.focus()
      }
    }}>
    <Sheet.Header><Sheet.Title>{m.model_changes_title()}</Sheet.Title></Sheet.Header>
    <div class="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto px-4 pb-4">
      <Field.FieldGroup>
        <Field.Field
          ><Field.FieldLabel for="history-provider">{m.common_model_service()}</Field.FieldLabel><Select.Root
            type="single"
            bind:value={providerId}
            ><Select.Trigger id="history-provider" class="w-full"
              >{serviceOptions.get(providerId) ?? (providerId || m.model_changes_all_services())}</Select.Trigger
            ><Select.Content
              ><Select.Group
                ><Select.Item value="">{m.model_changes_all_services()}</Select.Item
                >{#each [...serviceOptions] as [id, name] (id)}<Select.Item value={id}>{name}</Select.Item
                  >{/each}</Select.Group
              ></Select.Content
            ></Select.Root
          ></Field.Field>
        <Field.Field
          ><Field.FieldLabel for="history-type">{m.model_changes_type()}</Field.FieldLabel><Select.Root
            type="single"
            bind:value={type}
            ><Select.Trigger id="history-type" class="w-full"
              >{type ? changeLabel(type) : m.model_changes_all()}</Select.Trigger
            ><Select.Content
              ><Select.Group
                ><Select.Item value="">{m.model_changes_all()}</Select.Item
                >{#each changeTypes as kind (kind)}<Select.Item value={kind}>{changeLabel(kind)}</Select.Item
                  >{/each}</Select.Group
              ></Select.Content
            ></Select.Root
          ></Field.Field>
        <Field.Field
          ><Field.FieldLabel for="history-source">{m.model_changes_source()}</Field.FieldLabel><Select.Root
            type="single"
            bind:value={source}
            ><Select.Trigger id="history-source" class="w-full"
              >{source ? sourceLabel(source) : m.model_changes_all()}</Select.Trigger
            ><Select.Content
              ><Select.Group
                ><Select.Item value="">{m.model_changes_all()}</Select.Item
                >{#each ['automatic_sync', 'manual_sync', 'manual_change'] as origin (origin)}<Select.Item
                    value={origin}>{sourceLabel(origin as ModelChangeSource)}</Select.Item
                  >{/each}</Select.Group
              ></Select.Content
            ></Select.Root
          ></Field.Field>
        <Field.Field
          ><Field.FieldLabel for="history-search">{m.model_changes_search()}</Field.FieldLabel><Input
            id="history-search"
            bind:value={search} /></Field.Field>
      </Field.FieldGroup>
      {#if providers.isError}<RequestFailure
          message={localizeBackendErrorMessage(providers.error)}
          retry={() => providers.refetch()} />{/if}
      {#if selectedId}
        <div bind:this={selectedElement} tabindex="-1" aria-label={m.model_changes_details()}>
          {#if selected.isPending}<p role="status">
              {m.model_changes_loading()}
            </p>{:else if selected.isError}<RequestFailure
              message={localizeBackendErrorMessage(selected.error)}
              retry={() => selected.refetch()} />{:else if selected.data}{@render batch(selected.data)}{/if}
        </div>
      {/if}
      {#if history.isError}<RequestFailure
          message={localizeBackendErrorMessage(history.error)}
          retry={() => history.refetch()} />{/if}
      {#if history.isPending}<p role="status">
          {m.model_changes_loading()}
        </p>{:else if records.length === 0 && !history.isError}<Empty.Root
          ><Empty.Header
            ><Empty.Title>{filtered ? m.model_changes_no_match() : m.model_changes_empty()}</Empty.Title></Empty.Header
          ></Empty.Root
        >{:else}
        {#each records.filter((record) => record.id !== selectedId) as record (record.id)}{@render batch(record)}{/each}
      {/if}
      {#if history.hasNextPage}<Button
          variant="outline"
          disabled={history.isFetchingNextPage}
          onclick={() => history.fetchNextPage()}
          >{#if history.isFetchingNextPage}<Spinner data-icon="inline-start" />{/if}{m.model_changes_older()}</Button
        >{/if}
    </div>
  </Sheet.Content>
</Sheet.Root>
