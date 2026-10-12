import { describe, expect, test } from 'bun:test'

import { buildActivityGrid, buildLatencyChart } from '../src/lib/stats-chart'

const HOUR_MS = 3_600_000
const DAY_MS = 86_400_000

describe('latency chart', () => {
  test('missing latency points follow the requested coarse grain, not the base quarter hour', () => {
    const start = Date.UTC(2026, 8, 2, 18, 15)
    const points = buildLatencyChart(
      [
        { bucket_start: start, avg_first_token_ms: 0, avg_output_tps: null },
        { bucket_start: start + 8_100_000, avg_first_token_ms: null, avg_output_tps: 12 },
      ],
      2_700_000,
    )
    expect(points).toEqual([
      { bucket: new Date(start), firstToken: 0, outputTps: null },
      { bucket: new Date(start + 2_700_000), firstToken: null, outputTps: null },
      { bucket: new Date(start + 5_400_000), firstToken: null, outputTps: null },
      { bucket: new Date(start + 8_100_000), firstToken: null, outputTps: 12 },
    ])
  })
  test('breaks both series across buckets without requests', () => {
    const start = Date.UTC(2026, 8, 2, 18)
    const points = buildLatencyChart(
      [
        { bucket_start: start, avg_first_token_ms: 1_000, avg_output_tps: 50 },
        { bucket_start: start + 2 * HOUR_MS, avg_first_token_ms: 2_000, avg_output_tps: null },
        { bucket_start: start + 3 * HOUR_MS, avg_first_token_ms: null, avg_output_tps: 0 },
      ],
      HOUR_MS,
    )

    expect(points).toEqual([
      { bucket: new Date(start), firstToken: 1, outputTps: 50 },
      { bucket: new Date(start + HOUR_MS), firstToken: null, outputTps: null },
      { bucket: new Date(start + 2 * HOUR_MS), firstToken: 2, outputTps: null },
      { bucket: new Date(start + 3 * HOUR_MS), firstToken: null, outputTps: 0 },
    ])
  })

  test('keeps unique time coordinates when first and last buckets share a localized label', () => {
    // 跨日 24h 窗口返回 25 个小时桶，首尾同为 19:00；显示标签重名，
    // 但 x 坐标必须保持真实时刻，否则会折回左端画出斜线。
    const start = Date.UTC(2026, 8, 2, 19)
    const points = buildLatencyChart(
      Array.from({ length: 25 }, (_, i) => ({
        bucket_start: start + i * HOUR_MS,
        avg_first_token_ms: 1_000,
        avg_output_tps: 20,
      })),
      HOUR_MS,
    )

    expect(points).toHaveLength(25)
    expect(points[0].bucket.toISOString().slice(11, 16)).toBe('19:00')
    expect(points[24].bucket.toISOString().slice(11, 16)).toBe('19:00')
    const times = points.map((point) => point.bucket.getTime())
    expect(times).toEqual([...times].sort((a, b) => a - b))
    expect(new Set(times).size).toBe(times.length)
    expect(points.every((point) => point.bucket instanceof Date)).toBe(true)
  })
})

describe('activity grid', () => {
  test('absolute windows exclude both outside edges without extra cells', () => {
    const start = Date.UTC(2026, 8, 17, 8, 15)
    const grid = buildActivityGrid(
      [
        {
          bucket_start: start - 900_000,
          total_input_tokens: 500,
          total_output_tokens: 0,
          total_cache_read_tokens: null,
          total_cache_write_tokens: null,
        },
        {
          bucket_start: start,
          total_input_tokens: 10,
          total_output_tokens: 2,
          total_cache_read_tokens: null,
          total_cache_write_tokens: null,
        },
        {
          bucket_start: start + 900_000,
          total_input_tokens: null,
          total_output_tokens: null,
          total_cache_read_tokens: null,
          total_cache_write_tokens: null,
        },
        {
          bucket_start: start + 2_700_000,
          total_input_tokens: 500,
          total_output_tokens: 0,
          total_cache_read_tokens: null,
          total_cache_write_tokens: null,
        },
      ],
      { startMs: start, endMs: start + 2_700_000, bucketMs: 900_000 },
    )

    expect(grid.cells.map(({ start: time, tokens, level }) => [time, tokens, level])).toEqual([
      [start, 12, 4],
      [start + 900_000, null, 0],
      [start + 1_800_000, 0, 0],
    ])
    expect(grid.colStarts).toEqual([start])
    expect(grid.colCount).toBe(1)
    expect(grid.rowCount).toBe(3)
  })
  test.each([2_700_000, 18_900_000, 172_800_000, 777_600_000])(
    'absolute windows preserve all 512 arbitrary-grain buckets without position collisions (%i ms)',
    (bucketMs) => {
      const start = Date.UTC(2026, 8, 17, 8, 15)
      const last = start + 511 * bucketMs
      const grid = buildActivityGrid(
        [
          {
            bucket_start: last,
            total_input_tokens: 7,
            total_output_tokens: 5,
            total_cache_read_tokens: null,
            total_cache_write_tokens: null,
          },
        ],
        { startMs: start, endMs: start + 512 * bucketMs, bucketMs },
      )
      expect(grid.cells).toHaveLength(512)
      expect(grid.cells[0].start).toBe(start)
      expect(grid.cells[511].start).toBe(last)
      expect(grid.cells[511].tokens).toBe(12)
      expect(grid.cells.reduce((sum, cell) => sum + (cell.tokens ?? 0), 0)).toBe(12)
      expect(new Set(grid.cells.map((cell) => `${cell.col}:${cell.row}`)).size).toBe(512)
      expect(grid.colCount * grid.rowCount).toBeGreaterThanOrEqual(512)
    },
  )

  test('the last coarse bucket can cover a partial period without extending the selected interval', () => {
    const start = Date.UTC(2026, 8, 17, 8, 15)
    const grid = buildActivityGrid([], { startMs: start, endMs: start + HOUR_MS, bucketMs: 2_700_000 })
    expect(grid.cells.map((cell) => cell.start)).toEqual([start, start + 2_700_000])
  })

  test('repeated DST local labels keep separate activity cells and usage', () => {
    const start = Date.UTC(2026, 10, 1, 5)
    const repeated = start + HOUR_MS
    const label = new Intl.DateTimeFormat('en-US', {
      timeZone: 'America/New_York',
      hour: '2-digit',
      minute: '2-digit',
      hourCycle: 'h23',
    })
    expect(label.format(start)).toBe('01:00')
    expect(label.format(repeated)).toBe('01:00')
    const grid = buildActivityGrid(
      [
        {
          bucket_start: start,
          total_input_tokens: 3,
          total_output_tokens: 0,
          total_cache_read_tokens: null,
          total_cache_write_tokens: null,
        },
        {
          bucket_start: repeated,
          total_input_tokens: 7,
          total_output_tokens: 0,
          total_cache_read_tokens: null,
          total_cache_write_tokens: null,
        },
      ],
      { startMs: start, endMs: start + 2 * HOUR_MS, bucketMs: 900_000 },
    )
    expect(grid.cells).toHaveLength(8)
    expect(grid.cells.filter((cell) => cell.tokens !== 0).map((cell) => [cell.start, cell.tokens])).toEqual([
      [start, 3],
      [repeated, 7],
    ])
    expect(new Set(grid.cells.map((cell) => `${cell.col}:${cell.row}`)).size).toBe(8)
    expect(grid.colStarts).toEqual([start, repeated])
  })

  test.each([
    [100, 20, null, null, 120],
    [100, 20, 80, 10, 120],
    [100, null, null, null, 100],
    [null, 20, 80, 10, 20],
    [null, null, 80, 10, null],
    [null, null, null, null, null],
    [0, 0, 80, 10, 0],
  ])(
    'counts reported input/output without adding cache subsets (%s/%s/%s/%s)',
    (input, output, read, write, expected) => {
      const end = Date.UTC(2026, 8, 17)
      const grid = buildActivityGrid(
        [
          {
            bucket_start: end,
            total_input_tokens: input,
            total_output_tokens: output,
            total_cache_read_tokens: read,
            total_cache_write_tokens: write,
          },
        ],
        { startMs: end, endMs: end + DAY_MS, bucketMs: DAY_MS },
      )
      expect(grid.cells.find((cell) => cell.start === end)?.tokens).toBe(expected)
    },
  )

  test.each([900_000, HOUR_MS, 6 * HOUR_MS, DAY_MS])(
    'includes current bucket when the selected end is its exclusive boundary (%i ms)',
    (bucketMs) => {
      const current = Date.UTC(2026, 8, 17)
      const grid = buildActivityGrid([], { startMs: current - bucketMs, endMs: current + bucketMs, bucketMs })
      expect(grid.cells.map((cell) => [cell.start, cell.tokens])).toEqual([
        [current - bucketMs, 0],
        [current, 0],
      ])
    },
  )

  test('fills every bucket in the window and distinguishes zero and unknown usage', () => {
    const end = Date.UTC(2026, 8, 17)
    const grid = buildActivityGrid(
      [
        {
          bucket_start: end - DAY_MS,
          total_input_tokens: 10,
          total_output_tokens: null,
          total_cache_read_tokens: null,
          total_cache_write_tokens: null,
        },
        {
          bucket_start: end - 2 * DAY_MS,
          total_input_tokens: null,
          total_output_tokens: null,
          total_cache_read_tokens: null,
          total_cache_write_tokens: null,
        },
      ],
      { startMs: end - 3 * DAY_MS, endMs: end, bucketMs: DAY_MS },
    )

    expect(grid.cells.map((cell) => cell.start)).toEqual([end - 3 * DAY_MS, end - 2 * DAY_MS, end - DAY_MS])
    const byStart = new Map(grid.cells.map((cell) => [cell.start, cell]))
    expect(byStart.get(end - 3 * DAY_MS)?.tokens).toBe(0)
    expect(byStart.get(end - 2 * DAY_MS)?.tokens).toBeNull()
    expect(byStart.get(end - DAY_MS)?.tokens).toBe(10)
    expect(byStart.get(end - DAY_MS)?.level).toBe(4)
    expect(byStart.has(end)).toBe(false)

    const positions = new Set(grid.cells.map((cell) => `${cell.col}:${cell.row}`))
    expect(positions.size).toBe(grid.cells.length)
    expect(grid.rowCount).toBeLessThanOrEqual(7)
    expect(grid.colCount).toBe(grid.colStarts.length)
  })

  test('sub-day buckets group into parent-period columns', () => {
    const end = Date.UTC(2026, 8, 17, 12)
    const grid = buildActivityGrid([], { startMs: end - 6 * HOUR_MS, endMs: end, bucketMs: 900_000 })
    expect(grid.cells).toHaveLength(24)
    expect(grid.cells[0].start).toBe(end - 6 * HOUR_MS)
    expect(grid.cells[23].start).toBe(end - 900_000)
    expect(grid.rowCount).toBe(4)
    expect(grid.colCount).toBe(6)
    const positions = new Set(grid.cells.map((cell) => `${cell.col}:${cell.row}`))
    expect(positions.size).toBe(grid.cells.length)
  })
})
