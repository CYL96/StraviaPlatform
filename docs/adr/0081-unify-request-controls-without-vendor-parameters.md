---
status: accepted
---

# 请求控制只保留统一字段，不保留供应商专属参数

canonical 请求此前在 `RequestParameters` 中同时保存跨协议控制、按供应商命名的原生控制（`google_*`、`cohere_*`、`bedrock_*`、`anthropic_*` 等）、按 family 标记的原生工具 JSON（`HostedTool{family, definition}`）以及 Responses 显式 null 的保真状态。同一语义因此出现多种拼写，插件必须了解其它协议的字段，新增协议也会继续扩张参数集合。

## 决策

- 每个语义只有一个协议无关的字段。近义拼写在入口合并，例如 OpenAI `n` 与 Gemini `candidateCount` 合并为 `candidate_count`；`logprobs` 与 `top_logprobs` 合并为 `logprobs`；`user`、`safety_identifier` 与 Anthropic `metadata.user_id` 合并为 `end_user_id`；`thread_id`、`window_id` 与会话亲和 ID 合并为 `session.id`；`parent_thread_id` 并入 `session.parent_id`。
- 不再保存供应商专属控制。无法映射到统一字段的控制在入口静默丢弃，包括安全设置（Gemini safetySettings、Bedrock guardrail、Cohere safety_mode），以及只有单一供应商才理解的执行选项。
- 原生工具使用强类型 `NativeTool`：Web Search、代码执行、图像生成、工具搜索，以及 Responses 的 custom 与 namespace 工具。出口 codec 只编码目标协议能表达的变体，并使用官方类型名。
- 客户端会话头只提供统一的会话结构（会话、父会话、fork 来源、上下文窗口、turn）。安装 ID、turn state 等官方工具身份由插件私有状态生成或从上游响应获取，不再从客户端转发；`x-openai-subagent` 等客户端特有头直接丢弃。
- 显式 `null` 不再发往上游；它只记录在 `resets` 中，用于续接时阻止继承父请求的值。
- 旧持久化快照只经 runtime-contract 私有 legacy DTO 导入新字段；已安装的 format-1 插件继续由 SDK 兼容投影提供旧字段形态。WIT 版本与 canonical 格式版本不变。canonical 身份变化后，生成前缀命名空间升级，已有可复用前缀失效一次。

完整字段表见 [架构设计](../design/architecture.md) 的请求参数章节。

## Considered Options

- **保留供应商专属字段，只对它们做白名单。** 改动最小，但同一语义仍有多种拼写，插件仍需了解其它协议，违背单一 canonical 归属。
- **把无法统一的控制放进按协议分区的扩展袋。** 能保留更多原生能力，但等同于恢复参数透传，与正向白名单冲突。

## Consequences

- 直连原生 API 的客户端会失去未统一的能力，例如 Responses 的 file_search、mcp、computer 工具，Anthropic 的 bash、text_editor、memory 工具，Gemini 的安全阈值，以及 Bedrock guardrail。这些请求按上游默认行为执行。
- 新增上游时只需把统一字段投影到官方字段，不需要扩展 canonical。
- 升级后首个请求无法复用旧的生成前缀，需要重新建立。
