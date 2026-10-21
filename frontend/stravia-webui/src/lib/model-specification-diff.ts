import type { ModelSpecificationDiff } from '#lib/types.js'

export function specificationRows(diff: ModelSpecificationDiff): ModelSpecificationDiff[] {
  const rows: ModelSpecificationDiff[] = []
  function object(value: unknown): value is Record<string, unknown> {
    return value !== null && typeof value === 'object' && !Array.isArray(value)
  }
  function append(field: string, before: unknown, after: unknown): void {
    if ((before == null && after == null) || JSON.stringify(before) === JSON.stringify(after)) return
    if ((object(before) || before == null) && (object(after) || after == null)) {
      const keys = new Set([...Object.keys(before ?? {}), ...Object.keys(after ?? {})])
      if (keys.size > 0) {
        for (const key of keys) append(`${field}.${key}`, before?.[key], after?.[key])
        return
      }
    }
    // 模态与 Effort 列表保留整体语义；价格档位按条目展开，避免再次退回整段对象文本。
    if (
      (Array.isArray(before) || before == null) &&
      (Array.isArray(after) || after == null) &&
      ((before ?? []).some(object) || (after ?? []).some(object))
    ) {
      for (let index = 0; index < Math.max(before?.length ?? 0, after?.length ?? 0); index++)
        append(`${field}.${index}`, before?.[index], after?.[index])
      return
    }
    rows.push({ field, before, after })
  }
  append(diff.field, diff.before, diff.after)
  return rows
}
