import { describe, expect, test } from 'bun:test'

import { localizeBackendErrorMessage } from '$lib/backend-error'

describe('provider allowance backend errors', () => {
  test('localizes an unavailable saved provider without exposing the backend fallback', () => {
    const error = { code: 'PROVIDER_ALLOWANCE_UNAVAILABLE', message: 'provider allowance is unavailable' }

    expect(localizeBackendErrorMessage(error, 'en-US')).toBe(
      'Allowance data is no longer available for this model service.',
    )
    expect(localizeBackendErrorMessage(error, 'zh-CN')).toBe('此模型服务已无可用额度数据。')
  })

  test('tells the user which thinking rows to change', () => {
    const error = {
      code: 'THINKING_CONTROL_UNREPRESENTABLE',
      message: 'Target protocol cannot write this Target Thinking Control',
      params: {
        provider_id: 'ollama-cloud',
        model_id: 'gemma4:31b',
        levels: ['off', 'medium'],
        controls: ['disabled', 'enabled'],
        supported_controls: ['effort', 'hidden'],
      },
    }

    expect(localizeBackendErrorMessage(error, 'en-US')).toBe(
      'Open destination ollama-cloud/gemma4:31b. Thinking levels off, medium currently use Disable thinking, Enable thinking, which this model service cannot send. In the Thinking Level Map, change them to: Effort value, Level unsupported.',
    )
    expect(localizeBackendErrorMessage(error, 'zh-CN')).toBe(
      '打开请求目标 ollama-cloud/gemma4:31b。思考等级 off、medium 当前使用 明确关闭思考、仅开启思考，此模型服务无法发送。请在思考等级映射中改成：按强度值、此等级不支持。',
    )
  })
})

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
