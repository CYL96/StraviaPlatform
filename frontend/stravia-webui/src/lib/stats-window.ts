import type { StatsWindow } from '$lib/types/stats'

export const STATS_QUARTER_MS = 900_000

/** Live windows contain exactly spanMs of base buckets, including the open bucket. */
export function liveStatsWindow(spanMs: number, now = Date.now()): StatsWindow {
  const end_at = Math.floor(now / STATS_QUARTER_MS) * STATS_QUARTER_MS + STATS_QUARTER_MS
  return { start_at: end_at - spanMs, end_at }
}

export function localStatsDateTime(timestamp: number): string {
  const date = new Date(timestamp)
  const pad = (value: number) => String(value).padStart(2, '0')
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`
}

/** Reject normalization (DST gaps / invalid dates), and every repeated local wall time. */
export function parseLocalStatsDateTime(value: string): number | null {
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/.test(value)) return null
  const candidate = new Date(value).getTime()
  if (!Number.isFinite(candidate) || candidate <= 0 || candidate % STATS_QUARTER_MS !== 0) return null
  if (localStatsDateTime(candidate) !== value) return null
  // Search absolute instants around the candidate rather than assuming DST changes by one hour.
  // Local offsets can change by half an hour (or a day) and are not fixed over historical ranges.
  for (let delta = -26 * 60; delta <= 26 * 60; delta += 15) {
    if (delta !== 0 && localStatsDateTime(candidate + delta * 60_000) === value) return null
  }
  return candidate
}

export function statsPresentationBucket(spanMs: number, requestedSeconds: number): number {
  const base = Math.max(900, requestedSeconds)
  return Math.max(base, Math.ceil(spanMs / (base * 1_000 * 512)) * base)
}
