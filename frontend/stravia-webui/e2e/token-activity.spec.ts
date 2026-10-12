import { expect, test, type Page } from '@playwright/test'

import { prepareApp } from './prepare-app'

async function stubStats(page: Page): Promise<void> {
  const now = new Date(2026, 8, 17, 8, 7).getTime()
  await page.clock.setFixedTime(now)
  await page.route('**/api/v1/stats/overview**', async (route) => {
    await route.fulfill({
      json: {
        data: {
          total_requests: 128,
          total_input_tokens: 190_000_000,
          total_output_tokens: 8_600_000,
          total_cache_read_tokens: 320_000,
          total_cache_write_tokens: 12_000,
          total_reasoning_tokens: 44_000,
          avg_duration_ms: 120,
          avg_first_token_ms: 40,
          avg_output_tps: 25,
          error_count: 2,
        },
      },
    })
  })
  await page.route('**/api/v1/stats/series**', async (route) => {
    const url = new URL(route.request().url())
    const bucketMs = Number(url.searchParams.get('bucket') ?? '3600') * 1000
    const startAt = Number(url.searchParams.get('start_at'))
    const endAt = Number(url.searchParams.get('end_at'))
    expect(url.searchParams.has('hours')).toBe(false)
    expect(url.searchParams.has('tz_offset')).toBe(false)
    expect(startAt % 900_000).toBe(0)
    expect(endAt % 900_000).toBe(0)
    const data = []
    for (let i = 0; i < Math.min(12, Math.ceil((endAt - startAt) / bucketMs)); i++) {
      const start = endAt - (i + 1) * bucketMs
      data.push({
        bucket_ms: bucketMs,
        bucket_start: start,
        request_count: 4 + i,
        error_count: i === 3 ? 1 : 0,
        total_input_tokens: i === 5 ? null : i === 0 ? 190_000_000 : 12_000 + i * 37_000,
        total_output_tokens: i === 5 ? null : 800 + i * 1_900,
        total_cache_read_tokens: i === 5 ? null : i * 700,
        total_cache_write_tokens: i === 5 ? null : i * 90,
        total_reasoning_tokens: i * 40,
        avg_duration_ms: 100 + i * 8,
        avg_first_token_ms: 30 + i,
        avg_output_tps: 20 + i,
      })
    }
    await route.fulfill({ json: { data } })
  })
}

// Each selected interval supplies exactly its own buckets; layout must not invent older cells.
test('token activity grid stays within the selected interval at each granularity', async ({ page }) => {
  await prepareApp(page)
  await stubStats(page)
  await page.setViewportSize({ width: 1280, height: 900 })

  const ranges = [
    { label: 'Last 6h', cells: 24 },
    { label: 'Last 24h', cells: 24 },
    { label: 'Last 3d', cells: 12 },
    { label: 'Last 7d', cells: 7 },
  ]
  await page.goto('/stats')

  for (const range of ranges) {
    const selector = page.locator('[data-slot="select-trigger"]')
    await selector.click()
    await page.getByRole('option', { name: range.label }).click()
    await expect(selector).toContainText(range.label)
    const grid = page.getByRole('group', { name: 'Token activity' })
    await expect(grid).toBeVisible()
    // 方格尺寸由容器高度推导，等待测量后的重排稳定再断言。
    await expect(async () => {
      const metrics = await grid.evaluate((el) => {
        const wrapper = el.parentElement!.parentElement!
        return {
          cells: el.querySelectorAll('.token-activity-cell').length,
          scrolls: wrapper.scrollWidth > wrapper.clientWidth + 1 || wrapper.scrollHeight > wrapper.clientHeight + 1,
        }
      })
      expect(metrics.scrolls).toBe(false)
      expect(metrics.cells).toBe(range.cells)
    }).toPass()
  }
})

test('token activity cell tooltip reports the bucket time and token total', async ({ page }) => {
  await prepareApp(page)
  await stubStats(page)
  await page.goto('/stats')

  const grid = page.getByRole('group', { name: 'Token activity' })
  await expect(grid).toBeVisible()
  // 验证可见方格的悬浮提示包含用量。
  const newest = grid.locator('.token-activity-cell').last()
  await expect(newest).toHaveAttribute('aria-label', /tokens$/)
  await newest.hover()
  await expect(page.locator('[data-slot="tooltip-content"]')).toContainText('tokens')
})

test('token breakdown keeps net input without adding cache subsets back', async ({ page }) => {
  await prepareApp(page)
  await stubStats(page)
  await page.route('**/api/v1/stats/overview**', async (route) => {
    await route.fulfill({
      json: {
        data: {
          total_requests: 1,
          total_input_tokens: 20,
          total_output_tokens: 20,
          total_cache_read_tokens: 80,
          total_cache_write_tokens: 10,
          total_reasoning_tokens: null,
          avg_duration_ms: 1000,
          avg_first_token_ms: 500,
          avg_output_tps: 20,
          error_count: 0,
        },
      },
    })
  })
  await page.goto('/stats')
  const breakdown = page.getByRole('region', { name: 'Token breakdown', exact: true })
  await expect(breakdown).toBeVisible()
  await expect(breakdown.getByText('40', { exact: true })).toBeVisible()
  const categories = breakdown.getByRole('listitem')
  await expect(categories).toHaveCount(2)
})
