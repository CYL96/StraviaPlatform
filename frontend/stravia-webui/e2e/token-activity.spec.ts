import { expect, test, type Page } from '@playwright/test'

import { prepareApp } from './prepare-app'

const QUARTER = 900_000
const HOUR = 3_600_000
const END = Date.UTC(2026, 8, 17, 8, 15)
const PEAK = END - 9 * HOUR
const UNKNOWN = END - 2 * QUARTER
const MISSING = END - 3 * QUARTER

test.use({ timezoneId: 'UTC' })

async function stubStats(page: Page) {
  await page.clock.setFixedTime(END - 8 * 60_000)
  const requests: URL[] = []
  await page.route('**/api/v1/stats/**', async (route) => {
    const url = new URL(route.request().url())
    requests.push(url)
    const start = Number(url.searchParams.get('start_at'))
    const end = Number(url.searchParams.get('end_at'))
    if (url.pathname.endsWith('/overview')) {
      await route.fulfill({
        json: {
          data: {
            total_requests: 128,
            error_count: 2,
            total_input_tokens: 600,
            total_output_tokens: 86,
            total_cache_read_tokens: 320,
            total_cache_write_tokens: 12,
            total_reasoning_tokens: null,
            avg_duration_ms: 120,
            avg_first_token_ms: 40,
            avg_output_tps: 25,
          },
        },
      })
    } else if (url.pathname.endsWith('/series')) {
      const bucket = Number(url.searchParams.get('bucket')) * 1000
      const data = []
      // Aggregate durable quarter-hour facts at the requested grain, aligned to request start.
      for (let at = start; at < end; at += bucket) {
        let tokens = 0
        let known = false
        let traffic = false
        for (let quarter = at; quarter < Math.min(at + bucket, end); quarter += QUARTER) {
          if (quarter === MISSING) continue
          traffic = true
          if (quarter === UNKNOWN) continue
          known = true
          tokens += quarter === PEAK ? 1_000_000 : 100
        }
        if (!traffic) continue
        data.push({
          bucket_start: at,
          bucket_ms: bucket,
          request_count: 1,
          error_count: 0,
          total_input_tokens: known ? tokens : null,
          total_output_tokens: known ? 0 : null,
          total_cache_read_tokens: 80,
          total_cache_write_tokens: 10,
          total_reasoning_tokens: null,
          avg_duration_ms: 120,
          avg_first_token_ms: 40,
          avg_output_tps: 25,
        })
      }
      await route.fulfill({ json: { data } })
    } else {
      await route.fulfill({ json: { data: [] } })
    }
  })
  return requests
}

async function expectCoverage(page: Page, selectedStart: number, end: number, requests: URL[]) {
  const wrapper = page.locator('[data-activity-start]')
  const cells = page.getByRole('group', { name: 'Token activity' }).locator('.token-activity-cell')
  await expect(cells.first()).toBeVisible()
  await expect(async () => {
    const plan = await wrapper.evaluate((el) => ({
      start: Number(el.getAttribute('data-activity-start')),
      end: Number(el.getAttribute('data-activity-end')),
      bucket: Number(el.getAttribute('data-activity-bucket')),
      overflows: el.scrollWidth > el.clientWidth + 1,
    }))
    expect(plan.start).toBeLessThanOrEqual(selectedStart)
    expect(plan.end).toBe(end)
    expect(plan.bucket % QUARTER).toBe(0)
    expect(plan.overflows).toBe(false)
    const starts = await cells.evaluateAll((els) => els.map((el) => Number(el.getAttribute('data-start'))))
    expect(starts.length).toBeLessThanOrEqual(512)
    expect(starts).toEqual(
      Array.from({ length: (end - plan.start) / plan.bucket }, (_, i) => plan.start + i * plan.bucket),
    )
    expect(starts.at(-1)! + plan.bucket).toBe(end)
    expect(
      requests.some(
        (url) =>
          url.pathname.endsWith('/series') &&
          Number(url.searchParams.get('start_at')) === plan.start &&
          Number(url.searchParams.get('end_at')) === end &&
          Number(url.searchParams.get('bucket')) * 1000 === plan.bucket,
      ),
    ).toBe(true)
  }).toPass()
  return { wrapper, cells }
}

test('presets retain base grain while real history fills the available width', async ({ page }) => {
  await prepareApp(page)
  const requests = await stubStats(page)
  await page.setViewportSize({ width: 1600, height: 900 })
  await page.goto('/stats')
  for (const [label, hours, bucket] of [
    ['Last 6h', 6, QUARTER],
    ['Last 24h', 24, HOUR],
    ['Last 3d', 72, 6 * HOUR],
    ['Last 7d', 168, 24 * HOUR],
  ] as const) {
    await page.locator('[data-slot="select-trigger"]').click()
    await page.getByRole('option', { name: label, exact: true }).click()
    const { wrapper } = await expectCoverage(page, END - hours * HOUR, END, requests)
    await expect(wrapper).toHaveAttribute('data-activity-bucket', String(bucket))
    const latency = page.getByLabel('Latency and speed chart', { exact: true })
    await expect(latency).toBeVisible()
    await expect(async () => {
      const activityBox = (await wrapper.boundingBox())!
      const latencyBox = (await latency.boundingBox())!
      expect(Math.abs(activityBox.y - latencyBox.y)).toBeLessThanOrEqual(1)
      expect(Math.abs(activityBox.y + activityBox.height - latencyBox.y - latencyBox.height)).toBeLessThanOrEqual(1)
      expect(await wrapper.evaluate((el) => el.scrollHeight > el.clientHeight)).toBe(false)
    }).toPass()
  }
})

test('historical peak sets the whole grid scale, missing buckets are zero and null stays unknown', async ({ page }) => {
  await prepareApp(page)
  const requests = await stubStats(page)
  await page.setViewportSize({ width: 1600, height: 900 })
  await page.goto('/stats')
  await page.locator('[data-slot="select-trigger"]').click()
  await page.getByRole('option', { name: 'Last 6h', exact: true }).click()
  const selectedStart = END - 6 * HOUR
  const { wrapper } = await expectCoverage(page, selectedStart, END, requests)
  expect(Number(await wrapper.getAttribute('data-activity-start'))).toBeLessThan(PEAK)
  const peak = page.locator(`.token-activity-cell[data-start="${PEAK}"]`)
  await expect(peak).toHaveAttribute('aria-label', /1M tokens$/)
  await expect(peak).toHaveAttribute('data-level', '4')
  const newest = page.locator(`.token-activity-cell[data-start="${END - QUARTER}"]`)
  await expect(newest).toHaveAttribute('aria-label', /100 tokens$/)
  await expect(newest).toHaveAttribute('data-level', '1')
  await expect(page.locator(`.token-activity-cell[data-start="${MISSING}"]`)).toHaveAttribute('aria-label', /0 tokens$/)
  await expect(page.locator(`.token-activity-cell[data-start="${UNKNOWN}"]`)).toHaveAttribute(
    'aria-label',
    /token usage unknown$/,
  )
  for (const endpoint of ['overview', 'models', 'providers', 'api-keys']) {
    expect(
      requests.some(
        (url) =>
          url.pathname.endsWith(`/${endpoint}`) &&
          Number(url.searchParams.get('start_at')) === selectedStart &&
          Number(url.searchParams.get('end_at')) === END,
      ),
    ).toBe(true)
  }
  expect(
    requests.some(
      (url) =>
        url.pathname.endsWith('/series') &&
        url.searchParams.get('bucket') === '3600' &&
        Number(url.searchParams.get('start_at')) === selectedStart &&
        Number(url.searchParams.get('end_at')) === END,
    ),
  ).toBe(true)
  const openTooltip = page.locator('[data-slot="tooltip-content"][data-state$="open"]')
  await newest.hover()
  await expect(openTooltip).toContainText('100 tokens')
  await expect(openTooltip).toContainText('08:00')
  await page.locator(`.token-activity-cell[data-start="${UNKNOWN}"]`).hover()
  await expect(openTooltip).toContainText('token usage unknown')
})

test('widening the viewport queries and renders more earlier history', async ({ page }) => {
  await prepareApp(page)
  const requests = await stubStats(page)
  await page.setViewportSize({ width: 1280, height: 900 })
  await page.goto('/stats')
  const { wrapper, cells } = await expectCoverage(page, END - 24 * HOUR, END, requests)
  const before = Number(await wrapper.getAttribute('data-activity-start'))
  const count = await cells.count()
  await page.setViewportSize({ width: 1920, height: 900 })
  await expect.poll(async () => Number(await wrapper.getAttribute('data-activity-start'))).toBeLessThan(before)
  await expectCoverage(page, END - 24 * HOUR, END, requests)
  expect(await cells.count()).toBeGreaterThan(count)
})

test('narrow screens coarsen a long custom range without truncating or horizontal scrolling', async ({ page }) => {
  await prepareApp(page)
  const requests = await stubStats(page)
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/stats')
  await page.getByRole('button', { name: 'Date and time range', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('Start', { exact: true }).fill('2024-01-01T00:00')
  await dialog.getByLabel('End', { exact: true }).fill('2026-01-01T00:00')
  await dialog.getByRole('button', { name: 'Apply range', exact: true }).click()
  const { wrapper } = await expectCoverage(page, Date.UTC(2024, 0, 1), Date.UTC(2026, 0, 1), requests)
  expect(Number(await wrapper.getAttribute('data-activity-bucket'))).toBeGreaterThan(2 * 24 * HOUR)
  expect(
    await page.evaluate(() => document.documentElement.scrollWidth > document.documentElement.clientWidth + 1),
  ).toBe(false)
})

for (const scenario of [
  {
    name: 'includes net input, output, and both cache categories',
    input: 20,
    output: 20,
    read: 80,
    write: 10,
    total: '130',
    shares: [
      ['Input Tokens', '15.38%'],
      ['Output Tokens', '15.38%'],
      ['Cache read tokens', '61.54%'],
      ['Cache write tokens', '7.69%'],
    ],
  },
  {
    name: 'renders cache-only usage without inventing unreported input or output',
    input: null,
    output: null,
    read: 80,
    write: 10,
    total: '90',
    shares: [
      ['Cache read tokens', '88.89%'],
      ['Cache write tokens', '11.11%'],
    ],
  },
  {
    name: 'keeps reported zero categories distinct from unreported categories',
    input: 0,
    output: null,
    read: 80,
    write: 0,
    total: '80',
    shares: [
      ['Input Tokens', '0%'],
      ['Cache read tokens', '100%'],
      ['Cache write tokens', '0%'],
    ],
  },
] as const) {
  test(`token breakdown ${scenario.name}`, async ({ page }) => {
    await prepareApp(page)
    await stubStats(page)
    await page.route('**/api/v1/stats/overview**', async (route) => {
      await route.fulfill({
        json: {
          data: {
            total_requests: 1,
            total_input_tokens: scenario.input,
            total_output_tokens: scenario.output,
            total_cache_read_tokens: scenario.read,
            total_cache_write_tokens: scenario.write,
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
    await expect(breakdown.getByText(scenario.total, { exact: true })).toBeVisible()
    const legend = breakdown.getByRole('listitem')
    await expect(legend).toHaveCount(scenario.shares.length)
    for (const [label, share] of scenario.shares) {
      const category = legend.filter({ has: page.getByText(label, { exact: true }) })
      await expect(category.getByText(label, { exact: true })).toBeVisible()
      await expect(category.getByText(share, { exact: true })).toBeVisible()
    }
  })
}
