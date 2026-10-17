# 媒体生成

本文记录媒体生成的功能契约，不作为验证结果报告。架构取舍见 [ADR-0061](../adr/0061-bind-media-generation-to-routes.md)，领域术语见 [CONTEXT.md](../../CONTEXT.md)。

## 1. 范围与入口

Media Generation 的用户可见名称为“媒体生成”，属于 Advanced Capability。统一工具名为 `generate`；本期只支持图片生成与参考图编辑，首个接入后端为 Codex，不提前公布音频或视频类型。

工具通过平台执行，支持显式调用、MCP 及可选透明注入。媒体生成使用独立平台总开关，关闭时所有入口均不可执行。总开关开启后，所有有效 API Key 可显式调用；MCP 另受既有 MCP 开关约束。API Key 增加“媒体生成”透明注入选项，新建及已有 Key 均默认不勾选；该选项只控制自动暴露，不是能力授权。沿用 [ADR-0016](../adr/0016-gate-advanced-capabilities-and-separate-transparent-injection.md)。

## 2. 工具输入

按 `type` 与 `input` 建立判别联合，不采用任意 JSON 参数袋，不将不同媒体类型的所有字段平铺为可选项。

```ts
type GenerateInput = {
  type: "image";
  input: {
    prompt: string;
    aspect_ratio?: "1:1" | "3:4" | "4:3" | "9:16" | "16:9";
    resolution?: "1K" | "2K" | "4K";
    reference_images?: string[];
  };
};
```

- `type`、`input`、非空 `input.prompt` 必填；未知类型和未知字段明确拒绝。
- `aspect_ratio`、`resolution` 为生成偏好，不保证精确输出；省略时采用当前后端默认行为。
- 比例、分辨率档位及上游尺寸字段的转换由对应 Provider 的媒体生成适配实现拥有，允许近似映射。统一工具不维护供应商映射表，不新增 `image_size` 或供应商专用字段。
- `reference_images` 省略或为空表示文字生图；有参考图时按提示词执行编辑或参考生成。数组顺序对应“图 1、图 2……”；不得静默忽略或截断参考图，超出后端能力时明确失败。
- 不接受工具级 `model`、`provider`、`route`、`count`、`action` 或历史图片数量索引。每次成功返回一张图片。
- 未来新增类型时增加其专属输入分支；不要求音频、视频沿用图片的 `prompt` 或参考图角色，也不改变已有图片字段语义。

结构示例（引用 ID 为占位，不是有效调用值）：

```json
{
  "type": "image",
  "input": {
    "prompt": "保留参考图中的小屋，将背景改为雪山，水彩风格",
    "aspect_ratio": "16:9",
    "resolution": "2K",
    "reference_images": ["stravia://artifacts/<artifact-id>"]
  }
}
```

## 3. 文件输入与输出

参考图接受所属 Principal 的 Artifact Reference（`stravia://artifacts/<artifact-id>`）或公网 HTTP(S) 图片 URL，不接受客户端本地路径。客户端本地文件先通过现有上传入口取得引用；工具不另设 base64 或上传参数。

- Artifact Reference 必须校验当前 Principal 的归属、保留期及图片内容可用性，引用本身不授予访问权。
- 公网图片 URL 必须沿用既有网络安全与大小限制，先收存为当前 Principal 的 Artifact，再开始上游生成；收存失败不透传原 URL、不忽略附件继续执行。
- 输入是图片来源，不承载 `StraviaRead` 的 `question`、`download` 等操作选项。
- 向 Provider 交付输入内容时复用现有媒体传输规则，不把 `stravia://` 引用直接交给上游，也不将客户端或服务端本地路径作为上游可读地址。
- 上游返回的图片必须完整收存为当前 Principal 的 Artifact 后才能成功。上游 URL、base64 和本地存储路径不是工具的公开产物身份。

成功结果：

```ts
type ImageGenerateOutput = {
  path: string; // stravia://artifacts/<artifact-id>
  mime_type: string;
  size: number; // 文件字节数
  media: {
    width: number; // 实际像素宽度
    height: number; // 实际像素高度
  };
};
```

`media.width` 与 `media.height` 从实际生成文件读取，不以请求参数代填。结果不重复返回裸 Artifact ID，也不重复返回推算比例或分辨率档位。其他媒体类型的 `media` 字段随对应能力定义。

下载使用既有 `StraviaRead({path: "stravia://artifacts/<artifact-id>?download=1"})` 获取限时下载授权，不将下载地址当作稳定身份。保留期、归属、上传与下载授权沿用 [ADR-0048](../adr/0048-separate-artifact-references-from-transfer-grants.md)、[ADR-0049](../adr/0049-snapshot-explicit-media-inputs-as-artifacts.md) 和 [ADR-0051](../adr/0051-disambiguate-artifact-download-and-understanding.md)。

## 4. Route 与 Codex 接入

管理员按生成类型分别绑定 Route，当前仅有图片绑定。调用方不能覆盖该绑定。Route 持久化单一的 `output_modality`，当前为 `text` 或 `image`；新建表单默认文字，管理创建请求必须显式提交用途。Provider Model 可以同时登记文字和图片输出，能接收图片不等于能生成图片。这一用途约束取代先前不引入 Route 输出类型的限制，但不恢复已移除的协议 dispatcher route-type gate，也不改变普通推理与托管图片生成的协议含义。

候选及写入资格以 Provider Model 原始持久化快照的输出登记为准；缺失模态、缺失输出、显式空输出列表和仅登记其他输出均不赋予文字或图片资格。既有读取场景的文字回退不是能力登记；准备流程已实际保存的文字输出则按真实登记处理。Effective Availability 等既有约束继续生效，强制可用不能绕过模态要求。Core 统一校验所有管理入口；新增绑定和切换用途时，所有提交保留的 Target（包括禁用项）均须匹配。允许同次替换失配项，不自动删除绑定，失败不更改原配置且编辑器保留草稿。

已有 Target 的读取投影提供 `route_output_modalities`，由 Core 解析其模型身份（含历史别名）后按原始登记计算，不与当前可用性混合；新候选的 Provider Model 摘要仍同时约束可用性。历史 Target 可在保留原模型标识的情况下明确保存匹配用途，不要求客户端复制别名匹配或重写绑定。

只有 OneClick（一键接入）可为新 Route 推断用途：登记包含文字时优先创建文字路由（双输出也只创建一条），否则包含图片时创建图片路由，两者均无则拒绝。不添加 Route ID 后缀；已有 Route 以其已配置用途为准，不能通过重复绑定绕过待配置或失配检查。显式绑定只向已有 Route 增加 Target；Route 不存在时返回 `ROUTE_NOT_FOUND`，不隐式创建或推断用途。

图片生成绑定只接受已配置为图片、已启用且有可执行 Target 的 Route，并在候选读取、配置保存和每次执行前校验。图片输出声明不能替代真实媒体生成适配器资格。后续规格变化导致输出模态失配时，保留 Target 并显示原因，执行时先将其排除；其余合格 Target 继续按既有调度、重试和切换策略执行，全部失配或无可执行 Target 时在上游发送前明确失败。登记修复后原绑定可恢复资格。仅输出模态失配采用该排除规则：对剩余已启用 Target 仍执行原有适配器检查，其他适配器不兼容仍使媒体路由校验或执行失败，不静默跳过；禁用 Target 不参与该适配器检查。

输出失配排除必须先于原生图片适配器的优先选择，避免失配的原生图片 Target 压制仍合格的托管图片工具桥接 Target。`media_generation_targets_output_modality_mismatch` 表示应修复输出登记或替换失配 Target；它不是启用状态错误，不应引导管理员重复启用已经启用的 Route 或 Target。

媒体生成配置对 Route 的引用即使总开关关闭也阻止将用途改为文字。必须先清空图片生成 Route 引用；管理页支持解除绑定并同时关闭功能，关闭总开关或删除 Target 均不等于解绑。失效旧引用保留供修复，仍允许关闭功能或明确解绑，不将管理员锁在无效配置中。

**升级迁移：SQLite 和 PostgreSQL 的 `0022_default_existing_routes_to_text.sql` 将全部未配置输出用途的历史 Route 统一设为文字，已明确配置的文字或图片用途保持不变。** 它覆盖尚未应用和已经应用 `0021_route_output_modality.sql` 的实例，不改写旧迁移 checksum。Route ID、展示名、Target、启用偏好、优先级、超时、重试及其他调度参数和图片生成引用均保留；不自动启用禁用项，也不修改 Provider Model 规格或绕过资格校验。历史图片生成引用若指向被回填为文字的 Route，须明确改为图片用途并修正失配目标，或更换、清空引用。重启不会重置已保存用途。升级前停机备份完整数据根和外部数据库；回退须恢复备份，不能让旧二进制打开升级后 schema。冻结基线和增量升级规则见 [ADR-0073](../adr/0073-cutover-to-single-baseline-schema.md)。

按照已确认的 [Vendor 插件设计](vendor-plugins.md)，媒体生成可以与推理、完整搜索由同一 Vendor Plugin 和 Provider 连接提供，也可以是插件唯一的能力。沿用 Provider Model → Target → Route；Provider Model 可表示纯图片模型，不以聊天能力为前提，普通聊天执行不得选择不具备其所需能力的 Target。

插件拥有供应商参数映射、请求及结果解析；宿主继续拥有公开 generate 工具、权限、输入处理、Artifact 收存与交付，不允许插件自行注册 MCP 工具。能力移除更新保留绑定并明确不可用；内置 `base` 自动更新不为此暂停确认，不兼容数据按格式版本差异自动重置；本地包更新和手动恢复仍须确认数据丢弃。兼容在途生成使用旧版本完成，不兼容更新取消生成并阻止迟到结果提交。

Codex 参考 [OMP image-gen.ts](https://github.com/can1357/oh-my-pi/blob/main/packages/coding-agent/src/tools/image-gen.ts) 的实现方式：

1. 通过 Route 选择 Target，复用相应 Codex Provider 的账号、认证和连接设置。
2. Target 上游模型是支持托管图片工具的 GPT 对话模型。
3. 调用 Responses，以 `tool_choice` 强制选择 `image_generation`；参考图决定生成或编辑操作。
4. 消费流式响应，提取图片，收存并返回 Artifact 引用及实际规格。

不运行 Codex CLI，不采用独立的 `images/generations` 或 `images/edits` 调用链，不切换为另行配置的 OpenAI API Key。实际支持的模型、参数映射及错误分类须以选定链路的源码与协议证据核定；目录声明不能代替接入资格验证，保存配置也不代表已经执行过真实生图验证。

## 5. 失败与重试

沿用普通推理的可重试错误分类与 Route 重试、Target 切换策略，参见 [ADR-0034](../adr/0034-layer-route-target-selection.md)。不因请求已发送且执行状态不明而额外阻止原策略允许的重试；明确接受重复生成、重复消耗额度的风险，不承诺恰好执行一次。

- 不将参数校验、参考图读取或收存失败归类为可重试上游错误。
- 不将用户取消重新归类为重试条件；已有输出提交边界继续适用。
- 上游没有有效图片、输出图片无效或 Artifact 收存失败，均不能报告成功。
- 输出图片已生成但本地收存失败时，不以恢复文件保存为由重新调用生成。
- 使用现有工具错误通道，不返回 `success: true` 的空图结果，不把失败伪装为异步任务成功。

## 6. 管理面范围

遵循根 [DESIGN.md](../../DESIGN.md)，在“高级功能”中提供“媒体生成”配置页。本期仅提供能力总开关、图片生成 Route 选择与解绑、配置校验与当前支持类型的说明，不提供手动试生成、参考图上传、图片预览或图库。

图片路由选择器下方不重复展示资格说明或无候选空状态面板及其管理按钮。缺少路由时保留「未选择路由」，配置校验问题与恢复入口集中在上方已有告警；加载失败、保存失败和支持的媒体说明保持不变。

选择「未选择路由」是待保存的解绑操作：保存时清空 `image.route_id` 并同时关闭媒体生成，即使当前功能已开启也允许保存；提交前明确提示这一结果。仅改变选择、不保存不会解除引用。路由保存成功后刷新模型列表与详情的绑定状态，模型编辑器依据最新读取的引用判断占用，不把进入页面时的绑定快照当作当前事实，也不因刷新覆盖编辑草稿。

实际生成通过客户端工具调用、透明注入或 MCP 执行，不以管理员身份另建生成及 Artifact 所有权路径。配置不足必须说明缺失条件与补齐入口；配置保存、功能启用与真实请求成功是不同事实，不自动发送可能收费的上游测试请求。

## 7. 验收范围

实施时须通过产品入口验证以下已确认契约；本节是验收要求，不是已执行的测试报告：

- 统一输入严格按类型校验，图片生成与参考图编辑均可经 Codex 托管工具完成。
- 图片 Route 的绑定与运行前资格均校验；输出模态漂移保留绑定但排除执行，剩余合格 Target 可继续执行，其他适配器不兼容仍明确失败。
- SQLite 和 PostgreSQL 升级后，未配置用途的历史 Route 均自动归为文字；已明确保存的用途、身份、绑定和启停偏好保持不变。失效图片生成引用可关闭、解绑，关闭总开关不等于解绑。
- 平台 Gate、显式调用、MCP 与透明注入组合符合既有高级能力规则，已有 Key 不自动启用媒体生成注入。
- 文件归属、收存、保留期及签名下载沿用 Artifact 契约；无跨 Principal 引用和本地路径泄漏。
- 实际图片像素与返回的 `media` 一致；近似映射不伪报原请求尺寸。
- 错误分类、重试和切换符合已接受的重复执行风险边界，收存失败不重跑生成。
- 管理页仅包含本期配置功能；中英文、主题及窄屏行为遵循现有设计规范。

真实 Codex 生成验证会调用外部服务并消耗额度，必须在执行前取得针对测试账号与影响的明确授权；不能把本次设计讨论当作生产服务调用授权。
