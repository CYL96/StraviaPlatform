export interface StatsWindow {
  /** Paired UTC epoch milliseconds, covering [start_at, end_at). */
  start_at: number
  end_at: number
}

export interface GatewayStatus {
  status: string
  version: string
}

export interface StatsOverview {
  total_requests: number
  total_input_tokens: number | null
  total_output_tokens: number | null
  total_cache_read_tokens: number | null
  total_cache_write_tokens: number | null
  total_reasoning_tokens: number | null
  avg_duration_ms: number | null
  avg_first_token_ms: number | null
  /** 成功 Target attempts 的总输出 / 总完整耗时（tok/s）；任一样本缺输出或耗时为 null。 */
  avg_output_tps: number | null
  error_count: number
}

export interface StatsSeries {
  /** Effective presentation grain after whole-range coarsening, in milliseconds. */
  bucket_ms: number
  /** Absolute presentation bucket start (epoch ms), anchored to the selected start. */
  bucket_start: number
  request_count: number
  error_count: number
  total_input_tokens: number | null
  total_output_tokens: number | null
  total_cache_read_tokens: number | null
  total_cache_write_tokens: number | null
  total_reasoning_tokens: number | null
  avg_duration_ms: number | null
  avg_first_token_ms: number | null
  /** 与 overview 相同的完整覆盖口径，仅统计该时间桶的成功 Target attempts。 */
  avg_output_tps: number | null
}

export interface ModelStats {
  model_id: string | null
  deleted: boolean
  model: string
  request_count: number
  total_input_tokens: number | null
  total_output_tokens: number | null
  total_reasoning_tokens: number | null
  avg_duration_ms: number | null
}

export interface ProviderStats {
  provider_id: string | null
  deleted: boolean
  provider: string
  request_count: number
  error_count: number
  avg_duration_ms: number | null
  /** 已完成请求的输出 Token 总速（tok/s）；未完整报告输出或耗时为 null。 */
  avg_output_tps: number | null
}

export interface ApiKeyStats {
  deleted: boolean
  api_key_id: string
  api_key_name: string
  request_count: number
  total_input_tokens: number | null
  total_output_tokens: number | null
  cache_read_tokens: number | null
  cache_write_tokens: number | null
  reasoning_tokens: number | null
  last_used_at: number
}
