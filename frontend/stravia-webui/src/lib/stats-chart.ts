import type { StatsSeries } from '$lib/types'

type LatencyStats = Pick<StatsSeries, 'bucket_start' | 'avg_first_token_ms' | 'avg_output_tps'>
type TokenStats = Pick<
  StatsSeries,
  'bucket_start' | 'total_input_tokens' | 'total_output_tokens' | 'total_cache_read_tokens' | 'total_cache_write_tokens'
>

const HOUR_MS = 3_600_000
const DAY_MS = 86_400_000

export interface LatencyChartPoint {
  /** Bucket 起点的真实时刻；显示文案由轴刻度与 tooltip 按需本地化格式化。 */
  bucket: Date
  firstToken: number | null
  outputTps: number | null
}

export function buildLatencyChart(rows: readonly LatencyStats[], bucketMs: number): LatencyChartPoint[] {
  const points: LatencyChartPoint[] = []
  let previousStart: number | undefined

  for (const row of rows) {
    const start = row.bucket_start
    if (previousStart != null && Number.isFinite(start)) {
      for (let missing = previousStart + bucketMs; missing < start; missing += bucketMs) {
        points.push({ bucket: new Date(missing), firstToken: null, outputTps: null })
      }
    }
    points.push({
      bucket: new Date(start),
      firstToken: row.avg_first_token_ms == null ? null : row.avg_first_token_ms / 1000,
      outputTps: row.avg_output_tps,
    })
    previousStart = Number.isFinite(start) ? start : undefined
  }

  return points
}

export interface ActivityCell {
  /** Bucket 起点的真实 epoch ms；不按本地墙面时间重新对齐。 */
  start: number
  /** null 表示该时段有请求但 Token 用量未知；无请求为 0。 */
  tokens: number | null
  level: 0 | 1 | 2 | 3 | 4
  col: number
  row: number
}

export interface ActivityGridModel {
  cells: ActivityCell[]
  /** 每列首个 bucket 的真实时刻，用于本地化列标签。 */
  colStarts: number[]
  colCount: number
  rowCount: number
  bucketMs: number
}

/** 每列的目标跨度；列只包含完整粒度的 bucket，不以本地日历定义身份。 */
function activityColumnMs(bucketMs: number): number {
  if (bucketMs >= DAY_MS) return 7 * DAY_MS
  if (bucketMs >= 6 * HOUR_MS) return DAY_MS
  if (bucketMs >= HOUR_MS) return 6 * HOUR_MS
  return HOUR_MS
}

function tokenTotal(row: TokenStats): number | null {
  // 管理输入已扣除缓存读取；缓存分项不再加回输入与输出的合计。
  if (row.total_input_tokens == null && row.total_output_tokens == null) return null
  return (row.total_input_tokens ?? 0) + (row.total_output_tokens ?? 0)
}

/**
 * 展开绝对窗口 [startMs, endMs)：无数据的 bucket 占位，但不补窗口之外的边缘格。
 * 时间从选定起点自上而下再向右流动；本地标签重复不会合并 bucket。
 * 调用方选择有效 bucketMs，使整个窗口不超过 512 个 bucket。
 */
export function buildActivityGrid(
  rows: readonly TokenStats[],
  opts: { startMs: number; endMs: number; bucketMs: number },
): ActivityGridModel {
  const { startMs, endMs, bucketMs } = opts
  const totals = new Map<number, number | null>()
  for (const row of rows) totals.set(row.bucket_start, tokenTotal(row))

  // Even grains that do not divide an hour/day/week use distinct integer positions.
  const rowsPerColumn = Math.max(1, Math.ceil(activityColumnMs(bucketMs) / bucketMs))
  const colStarts: number[] = []
  const rawCells: Omit<ActivityCell, 'level'>[] = []
  let rowCount = 0
  let max = 0
  for (let start = startMs, index = 0; start < endMs; start += bucketMs, index++) {
    const col = Math.floor(index / rowsPerColumn)
    const row = index % rowsPerColumn
    if (row === 0) colStarts.push(start)
    rowCount = Math.max(rowCount, row + 1)
    const tokens = totals.has(start) ? totals.get(start)! : 0
    if (tokens != null) max = Math.max(max, tokens)
    rawCells.push({ start, tokens, col, row })
  }

  const cells = rawCells.map((cell): ActivityCell => ({
    ...cell,
    level:
      cell.tokens == null || cell.tokens === 0 || max === 0
        ? 0
        : (Math.min(4, Math.ceil((cell.tokens / max) * 4)) as 1 | 2 | 3 | 4),
  }))
  return { cells, colStarts, colCount: colStarts.length, rowCount, bucketMs }
}
