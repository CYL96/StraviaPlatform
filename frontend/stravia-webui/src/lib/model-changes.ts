import * as m from '#lib/paraglide/messages.js'
import { createContext } from 'svelte'
import type {
  ModelChangeSource,
  ModelChangeType,
  ProviderModelSyncSummary,
  ProviderModelSelectionPolicy,
} from '#lib/types.js'
import { toast } from 'svelte-sonner'
import { localizeBackendErrorMessage } from '#lib/backend-error.js'

export interface ModelChanges {
  show: (providerId?: string) => void
  reveal: (recordId: string) => void
}

// 根布局持有唯一侧栏，toast 的操作不随页签或页面卸载而失效。
export const [getModelChanges, setModelChanges] = createContext<ModelChanges>()

export const changeTypes: ModelChangeType[] = [
  'added',
  'missing',
  'restored',
  'deprecated',
  'deprecation_cleared',
  'specification_updated',
  'manual_added',
  'selection_policy_changed',
  'sync_failed',
]
export function modelChangeReason(reason: string): string {
  const reasons: Record<string, () => string> = {
    'Automatic synchronization is disabled': m.model_sync_pause_preference,
    'Provider is disabled': m.model_sync_pause_provider,
    'Provider credentials are invalid': m.model_sync_pause_credentials,
    'Provider plugin is unavailable': m.model_sync_pause_plugin,
    'Provider channel does not support model discovery': m.model_sync_pause_discovery,
    'Provider or model changed during discovery; check current settings and sync again': m.model_sync_error_conflict,
    'Discovery did not return a complete valid inventory; previous inventory retained': m.model_sync_error_inventory,
    'Discovery could not authenticate; check provider credentials': m.model_sync_error_credentials,
    'Model discovery was cancelled': m.model_sync_error_cancelled,
    'Model discovery or persistence failed; check provider settings and try again': m.model_sync_error_general,
    'Inventory saved; plugin recovery status could not be updated; check provider status': m.model_sync_error_partial,
  }
  for (const [raw, label] of Object.entries(reasons)) {
    if (reason === raw || reason.startsWith(`${raw};`) || reason.startsWith(`${raw}.`)) {
      const suffix = reason.slice(raw.length)
      const historyFailed =
        suffix === '; check history could not be saved' || suffix === '; postcommit status could not be saved'
      return label() + (historyFailed ? ` · ${m.model_sync_error_history()}` : suffix)
    }
  }
  return localizeBackendErrorMessage(reason)
}
export function changeLabel(type: ModelChangeType): string {
  return {
    added: m.model_changes_added,
    missing: m.model_changes_missing,
    restored: m.model_changes_restored,
    deprecated: m.model_changes_deprecated,
    deprecation_cleared: m.model_changes_deprecation_cleared,
    specification_updated: m.model_changes_specification_updated,
    manual_added: m.model_changes_manual_added,
    selection_policy_changed: m.model_changes_selection_policy_changed,
    sync_failed: m.model_changes_sync_failed,
  }[type]()
}
export function sourceLabel(source: ModelChangeSource): string {
  return {
    automatic_sync: m.model_changes_automatic,
    manual_sync: m.model_changes_manual_sync,
    manual_change: m.model_changes_manual_change,
  }[source]()
}
export function policyLabel(policy: ProviderModelSelectionPolicy): string {
  return policy === 'force_enabled'
    ? m.common_always_allow()
    : policy === 'force_disabled'
      ? m.common_don_t_allow()
      : m.common_use_synced_status()
}
export function specificationField(field: string): string {
  const labels: Record<string, () => string> = {
    display_name: m.model_changes_display_name,
    context: m.model_specification_context,
    input_modalities: m.model_specification_input_modalities,
    output_modalities: m.model_specification_output_modalities,
    reasoning_efforts: m.model_specification_reasoning_efforts,
    price: m.model_changes_price,
    input: m.provider_model_field_input,
    output: m.provider_model_field_output,
    cache_read: m.provider_model_field_cache_read,
    cache_write: m.provider_model_field_cache_write,
    context_over_200k: m.model_specification_extended_context_prices,
    type: m.model_changes_tier_type,
    size: m.model_changes_tier_size,
  }
  return field
    .split('.')
    .flatMap((key, index, path) => {
      if (key === 'tiers' || key === 'tier') return []
      if (path[index - 1] === 'tiers') return [m.provider_model_editor_tier_value({ index: Number(key) + 1 })]
      return [labels[key]?.() ?? key]
    })
    .join(' / ')
}
export function specificationValue(value: unknown): string {
  if (value == null) return m.model_specification_not_registered()
  if (Array.isArray(value)) return value.length ? value.map(specificationValue).join(', ') : m.model_changes_no_values()
  if (typeof value === 'object')
    return (
      Object.entries(value)
        .map(([key, entry]) => `${specificationField(key)}: ${specificationValue(entry)}`)
        .join(' · ') || '{}'
    )
  if (value === '') return m.model_changes_empty_value()
  if (typeof value === 'string') return value
  if (typeof value === 'number' || typeof value === 'boolean') return String(value)
  throw new TypeError('Model specification values must be JSON')
}
export function summaryDescription(summary: ProviderModelSyncSummary): string {
  return (['added', 'missing', 'restored', 'deprecated', 'deprecation_cleared', 'specification_updated'] as const)
    .filter((type) => summary[type] > 0)
    .map((type) => `${changeLabel(type)} ${summary[type]}`)
    .join(' · ')
}
export function syncToast(summary: ProviderModelSyncSummary, reveal: (id: string) => void, retry: () => void): void {
  const description = [summaryDescription(summary), summary.error ? modelChangeReason(summary.error) : null]
    .filter(Boolean)
    .join(' · ')
  const options = {
    description,
    ...(summary.record_id
      ? {
          action: { label: m.model_changes_details(), onClick: () => reveal(summary.record_id!) },
          ...(summary.outcome !== 'success' ? { cancel: { label: m.common_retry(), onClick: retry } } : {}),
        }
      : summary.outcome !== 'success'
        ? { action: { label: m.common_retry(), onClick: retry } }
        : {}),
  }
  if (summary.outcome === 'failure') toast.error(m.model_changes_failed(), options)
  else if (summary.outcome === 'partial_success') toast.warning(m.model_changes_partial(), options)
  else toast.success(description ? m.model_changes_saved() : m.model_changes_no_change(), options)
}
export function syncErrorToast(error: unknown, reveal: (id: string) => void, retry: () => void): void {
  const details = error && typeof error === 'object' && 'details' in error ? error.details : undefined
  const candidate =
    details && typeof details === 'object' && 'sync_summary' in details ? details.sync_summary : undefined
  // decodeAdmin attaches the server's typed saved-result DTO to Error.details.
  const summary = candidate as ProviderModelSyncSummary | undefined
  if (summary) syncToast(summary, reveal, retry)
  else
    toast.error(m.model_changes_failed(), {
      description: modelChangeReason(localizeBackendErrorMessage(error)),
      action: { label: m.common_retry(), onClick: retry },
    })
}
