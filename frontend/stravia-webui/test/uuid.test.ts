import { expect, spyOn, test } from 'bun:test'
import { createUuid } from '../src/lib/uuid'

test('UUID v4 fallback preserves random bytes while setting version, variant and zero padding', () => {
  const native = Object.getOwnPropertyDescriptor(crypto, 'randomUUID')
  Object.defineProperty(crypto, 'randomUUID', { configurable: true, value: undefined })
  const random = spyOn(crypto, 'getRandomValues').mockImplementation(
    <T extends ArrayBufferView | null>(array: T): T => {
      if (!(array instanceof Uint8Array) || array.length !== 16) throw new Error('Expected 16 random bytes')
      array.set([0, 1, 2, 3, 4, 5, 0xff, 7, 0xff, 9, 10, 11, 12, 13, 14, 15])
      return array
    },
  )
  try {
    expect(createUuid()).toBe('00010203-0405-4f07-bf09-0a0b0c0d0e0f')
  } finally {
    random.mockRestore()
    if (native) Object.defineProperty(crypto, 'randomUUID', native)
    else Reflect.deleteProperty(crypto, 'randomUUID')
  }
})
