import { describe, expect, test } from 'bun:test'
import { specificationRows } from '../src/lib/model-specification-diff'

describe('model history specification differences', () => {
  test('isolates changed prices and preserves zero versus missing values', () => {
    expect(
      specificationRows({
        field: 'price',
        before: { input: 0.3, output: 0.9, cache_read: 0.05, context_over_200k: null, tiers: [] },
        after: { input: 0, output: 0.9, cache_read: 0, cache_write: 0, tiers: [] },
      }),
    ).toEqual([
      { field: 'price.input', before: 0.3, after: 0 },
      { field: 'price.cache_read', before: 0.05, after: 0 },
      { field: 'price.cache_write', before: undefined, after: 0 },
    ])
  })

  test('keeps explicit empty lists distinct from unregistered specifications', () => {
    expect(specificationRows({ field: 'input_modalities', before: null, after: [] })).toEqual([
      { field: 'input_modalities', before: null, after: [] },
    ])
    expect(specificationRows({ field: 'reasoning_efforts', before: ['high'], after: [] })).toEqual([
      { field: 'reasoning_efforts', before: ['high'], after: [] },
    ])
  })

  test('retains nested tier changes and removed custom fields without unchanged prices', () => {
    expect(
      specificationRows({
        field: 'price',
        before: { tiers: [{ tier: { type: 'context', size: 200000 }, input: 2, custom: 7 }] },
        after: { tiers: [{ tier: { type: 'context', size: 200000 }, input: 4 }] },
      }),
    ).toEqual([
      { field: 'price.tiers.0.input', before: 2, after: 4 },
      { field: 'price.tiers.0.custom', before: 7, after: undefined },
    ])
  })
})
