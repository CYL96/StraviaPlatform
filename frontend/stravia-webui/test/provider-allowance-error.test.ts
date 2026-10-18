import { describe, expect, test } from 'bun:test'

import { localizeBackendErrorMessage } from '#lib/backend-error.js'

describe('route output recovery errors', () => {
  test('preserves Core error destination and route identities through encoded error messages', () => {
    const params = {
      provider_id: 'provider-unique-937',
      model_id: 'model-unique-482',
      route_id: 'route-unique-651',
      output_modality: 'image',
    }
    for (const locale of ['en-US', 'zh-CN'] as const) {
      for (const code of ['ROUTE_OUTPUT_MODALITY_MISMATCH', 'ROUTE_OUTPUT_MODALITY_UNREGISTERED']) {
        const error = new Error(JSON.stringify({ code, params, message: 'RAW_TARGET_SENTINEL' }))
        const message = localizeBackendErrorMessage(error, locale)
        expect(message).toContain(params.provider_id)
        expect(message).toContain(params.model_id)
        expect(message).not.toContain('RAW_TARGET_SENTINEL')
        if (code === 'ROUTE_OUTPUT_MODALITY_MISMATCH') {
          const textError = new Error(JSON.stringify({ code, params: { ...params, output_modality: 'text' } }))
          expect(message).not.toBe(localizeBackendErrorMessage(textError, locale))
        }
      }
      for (const code of ['ROUTE_IMAGE_GENERATION_BOUND', 'ROUTE_OUTPUT_MODALITY_UNCONFIGURED', 'ROUTE_NOT_FOUND']) {
        const error = new Error(JSON.stringify({ code, params, message: 'RAW_ROUTE_SENTINEL' }))
        const message = localizeBackendErrorMessage(error, locale)
        expect(message).toContain(params.route_id)
        expect(message).not.toContain('RAW_ROUTE_SENTINEL')
      }
    }
  })

  test('localizes historical and media errors without leaking backend fallback messages', () => {
    const codes = [
      'ROUTE_OUTPUT_MODALITY_UNCONFIGURED',
      'ROUTE_OUTPUT_MODALITY_UNREGISTERED',
      'ROUTE_OUTPUT_MODALITY_MISMATCH',
      'ROUTE_IMAGE_GENERATION_BOUND',
      'route_output_modality_unconfigured',
      'route_output_modality_unavailable',
      'target_output_modality_mismatch',
      'media_generation_route_output_modality_mismatch',
      'media_generation_targets_output_modality_mismatch',
    ]
    for (const locale of ['en-US', 'zh-CN'] as const) {
      for (const code of codes) {
        const error = { code, message: `UNLOCALIZED_BACKEND_FALLBACK_${code}` }
        expect(localizeBackendErrorMessage(error, locale)).not.toContain(error.message)
      }
      const drift = localizeBackendErrorMessage({ code: 'media_generation_targets_output_modality_mismatch' }, locale)
      const missing = localizeBackendErrorMessage({ code: 'media_generation_targets_missing' }, locale)
      expect(drift).not.toBe(missing)
    }
  })
})
