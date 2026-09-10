# 🐾 BaoClaw（中文文档）

[English](../README.md)

> 中文文档由英文文档翻译而来，可能落后于最新版本；以英文文档为准。

## 🐾 BaoClaw — 带持久记忆和多客户端访问的 AI 编程助手

BaoClaw 是一个开源 AI 编程 Agent，基于 Rust 核心引擎，具备持久记忆、本地多客户端会话共享、定时任务和实验性自我改进功能。它以守护进程方式运行，同时连接终端、Telegram 和 WhatsApp。

和那些关掉窗口就失忆的 Agent 不同，BaoClaw 会随着使用不断积累对你和你项目的了解。用得越多，越好用。

## 核心特性

### 🧠 持久记忆

- 项目级记忆 — 每个项目目录独立的 `memory.jsonl`
- 全局记忆 — 跨项目的个人偏好和决策
- 自动注入 — 记忆自动加载到系统提示词中
- 手动管理 — `/memory add`、`/memory list`、`/memory delete`

### 📱 全局守护进程，多客户端

- 一个守护进程管所有项目 — 单个 daemon 进程管理所有项目目录的会话
- 项目级会话 — 每个工作目录有独立的会话历史和记忆
- 多设备访问 — 在配置好的 gateway 可访问 daemon 时，可从其他设备继续任务
- 实时流式输出 — 所有客户端同步看到工具调用和响应
- 无冲突 — 两个终端在不同目录工作，使用不同会话，互不干扰
- 会话持久化 — 对话在守护进程重启后自动恢复，按项目目录绑定

### 🔄 自我进化引擎（实验性）

一个闭环学习循环：

- 轨迹记录 — 每次交互自动记录工具调用、结果和耗时
- Skill 自动生成 — 复杂的成功任务自动提取为可复用的 skill 候选
- 自我评估 — 每 15 个任务触发反思，创建或改进 skill
- 用户评价 — 对交互评分（good/bad），构建偏好数据
- 训练数据导出 — 导出可进一步整理为 DPO/RLHF 数据集的轨迹 JSONL
- 个人级进化 — skill 和轨迹跨项目积累（`~/.baoclaw/evolution/`）
- Evolve 工具 — Agent 可自主创建、改进和提升 skill

### ⏰ 定时任务

- 周期执行 — 在守护进程内自动运行预设的提示词
- 灵活调度 — `every 30m`、`every 2h`、`daily 09:00`、`weekly mon 09:00`
- 结果推送 — 定时任务结果推送到所有连接的客户端（终端 + Telegram）
- 持久化 — 任务保存在 `~/.baoclaw/cron.json`，守护进程重启后自动恢复
- 完整能力 — 每个任务都拥有完整的 Agent 工具访问权限

### 📄 文档问答

- 上传文件 — 通过 Telegram 或终端（`@file.pdf`）上传 PDF、DOCX、图片
- 文本提取 — DOCX 用 mammoth，PDF 用 pdf-parse
- 原生文档 — PDF 可直接发送给 Claude API
- 图片理解 — 支持 Anthropic 和 OpenAI 兼容 API 的多模态
- Tab 补全 — 终端中输入 `@` 后按 Tab 自动补全文件路径

### 🗂️ 项目级隔离

- `/cd` 命令 — 运行时切换工作目录，相当于切换项目
- 自动初始化 — 新目录自动创建 `.baoclaw/` 配置骨架
- 项目绑定会话 — 每个目录对应独立的持久化会话文件
- 自动恢复 — 重连时自动恢复项目的对话历史
- 项目指令 — `BAOCLAW.md` 按项目加载到系统提示词
- 记忆隔离 — 每个项目有独立的记忆存储

### 🛠️ 内置工具

Bash、文件读写编辑、Grep、Glob、Web 搜索、Web 抓取、记忆管理、子 Agent、自我进化、Todo、Notebook 编辑、项目笔记、工具搜索等。

### 🔌 可扩展

- MCP 协议 — 连接外部 MCP 服务器获取更多工具
- Skills — Markdown 格式的技能文件（个人级 + 项目级）
- 插件系统 — 目录式插件，包含工具、技能和 MCP 配置
- 200+ 模型 — Anthropic 原生 + 任意 OpenAI 兼容 API

### 🔁 模型降级

- 自动重试 — 限流时指数退避重试
- 降级链 — 配置多个模型，限流时自动切换
- 透明提示 — 终端实时显示模型切换

### ⌨️ 快捷键

- `Ctrl+C`（任务中）→ 中止当前任务
- `Ctrl+C`（空闲时）→ 提示再按一次退出
- `Ctrl+C × 2` → 断开连接
- `Tab` → 自动补全命令和文件路径

### 🚀 v2.0 — 智能引擎层（全新）

Phase 2–4 新增特性，让 BaoClaw 更聪明、更安全、更快速：

#### 🔍 跨会话搜索（#5）

- **SQLite + FTS5** 全文检索所有历史会话
- 关键词搜索，返回带上下文片段的排序结果
- 3 周前看到的解决方案，秒级找到

#### ❄️ 冻结快照缓存（#6）

- 系统提示词和工具列表只在会话开始时构建一次，然后**冻结**
- 最大化 Anthropic prompt cache 命中率——每 turn 只有动态提醒变化
- 每次调用都省成本、降延迟

#### 👤 用户画像（#7）

- `~/.baoclaw/USER.md` — 持久化用户画像（姓名、语言、编码风格、工具偏好）
- 自动注入系统提示词，实现个性化回复
- 会话统计自动合并（总轮次、费用、常用工具）

#### 🔄 Skill 自改进闭环（#8）

- **5 阶段循环**：采集 → 评估 → 改进 → 验证 → 退役
- 基于相关率、成功率、用户评分和时效性综合评分
- 自动退役持续低效的 skill，对平庸 skill 生成改进建议
- 定期运行，保持 skill 集健康

#### 📐 自适应 Compact（#9）

- `AdaptiveCompactTracker` 根据压缩历史学习最优 `keep_recent` 参数
- 用户重复问压缩前内容 → 增大 `keep_recent`（保留更多）
- 压缩率差且无信息丢失 → 减小 `keep_recent`（压缩更激进）
- 范围 6–30 条消息，每会话自动调整

#### 🏥 工具健康监控（#10）

- 实时追踪每个工具的成功/失败/超时率
- **三级状态**：健康 → 降级（连续 3 次失败）→ 禁用（连续 6 次）
- 降级工具在系统提示词中显示警告信息
- 连续 5 次成功后自动恢复

#### 🎯 意图预测（#11）

- 根据消息关键词预测用户意图（编码、调试、测试、重构、Git、研究…）
- 转移矩阵学习意图→意图的先后关系（如 编码→测试）
- 高置信度预测触发工具预加载提示

#### 🧮 上下文窗口智能分配（#12）

- 注意力评分 = 0.5×相关度 + 0.3×时效性 + 0.2×频率
- 必选块（系统提示词、工具）始终包含
- 可选块（记忆、skill、搜索结果）按评分贪心填充
- 预算超限时优先裁剪低分块

#### 🏖️ 沙箱执行（#13）

- 三种后端：**Bubblewrap**（Linux 命名空间）→ **Docker**（容器）→ 无沙箱（直接执行）
- 启动时自动检测最佳可用后端
- 可配置：读写挂载、网络隔离、内存/CPU 限制、超时
- 一行 `wrap_command()` 调用即可沙箱化任意命令

#### 🛡️ Prompt 注入检测（#14）

- **20 种模式**覆盖 6 大类：指令覆写、角色劫持、数据外泄、编码技巧、隐藏载荷、越狱
- 启发式评分，多匹配递减收益 + 跨类别加成
- 四级严重度：干净 → 可疑 → 危险 → 致命
- `sanitize()` 方法用 `[REDACTED]` 替换检测到的模式

#### 🔐 子代理深度策略（#15）

- 最大嵌套深度：3 层
- **逐层工具收紧**：Depth 0=全部工具，Depth 1=安全工具，Depth 2=只读，Depth 3=最小权限（仅 FileRead+Bash）
- 每层预算：轮次上限（100→30→15→5）、费用上限（$10→$2→$0.50→$0.10）
- 预算耗尽 → 自动终止子代理

#### 📡 流式工具执行器（#16）

- 实时分块输出：启动 → 进度 → 标准输出 → 标准错误 → 完成 → 错误 → 心跳
- `StreamWriter` / `StreamReader` 对，基于 `tokio::sync::mpsc`
- 可配置超时（默认 5 分钟）、缓冲区大小、最大输出（默认 1MB）
- 通过 `tokio::select!` 并发读取 stdout 和 stderr

### 🚀 v2.1 — 进化引擎（全新）

#### 📋 工作流模板引擎（#17）

- **5 个内置模板**：`code_review`、`bug_fix`、`feature`、`docs`、`refactor`
- 触发器匹配（`/review` → code_review 模板）
- 变量替换：`${variable}` 语法 + 步骤输出引用 `${stepN.output}`
- 条件工作流步骤（`condition` 字段）
- JSON 导入/导出模板，方便分享
- 支持创建自定义模板，定义专属工作流和变量

#### 🌿 Git 集成（#18）

- **分支管理**：创建、列出、切换、合并，支持名称验证和保护分支检测
- **提交管理**：暂存文件、使用约定式提交格式（`feat:`、`fix:`、`chore:`）、修改、撤销
- **冲突解决**：从合并标记检测冲突，以 ours/theirs 方式解决
- **PR 管理**：创建、按状态列出、审查、合并
- SSH 和 HTTPS 凭证管理，支持按主机查找

#### 🧭 模型路由（#19）

- **智能路由**：按任务类型选择模型（编码/补全/创意/分析）
- **成本感知**：简单任务优先便宜模型，复杂任务路由到高级模型
- **预算追踪**：设置消费上限，追踪 token 用量，超阈值告警
- **用量学习**：记录路由历史，基于使用模式生成优化建议
- **降级链**：主模型不可用时自动切换备选模型

#### 📊 遥测与监控（#20）

- **事件采集**：记录工具调用、模型请求、错误、会话事件
- **趋势分析**：检测时间窗口内上升/下降/稳定模式
- **多格式导出**：JSON 用于程序化处理，CSV 用于电子表格分析
- **聚合统计**：每工具使用次数、模型分布、错误率

#### 🔐 权限门禁（#21）

- **工具级访问控制**：按会话对每个工具进行授予/撤销权限
- **交互式提示**：在执行敏感操作前请求用户审批
- **权限缓存**：带 TTL 的决策缓存，避免频繁提示
- **默认拒绝模式**：初始全部工具拒绝，按需显式授权

### 🖥️ CLI 和 TUI

- **18 个新 CLI 命令**，覆盖 5 个模块（`/template`、`/git`、`/model`、`/telemetry`、`/permission`）
- **终端 UI（TUI）**，基于 Ink（React 终端框架）构建：
  - 分栏布局：消息列表 + 流式输出
  - 工具执行面板，实时状态显示
  - 语法高亮代码块
  - 快捷键帮助面板（`Ctrl+H`）
- **Unix socket IPC**：基于 Unix 域套接字的 JSON-RPC 2.0 + NDJSON 流
- 自动发现 daemon socket：优先使用固定 socket（Linux 为 `$XDG_RUNTIME_DIR/baoclaw.sock`，macOS 为 `/tmp/baoclaw-sockets/baoclaw.sock`），再回退到 cwd-hash socket

## 内部机制：引擎工作原理

本节描述 BaoClaw 的五个核心机制：**记忆**、**上下文**、**进化**、**系统提示词**和**模型退回**。全部在 Rust 核心引擎（`baoclaw-core/src/engine/`）中实现。

---

### 1. 🧠 记忆机制

BaoClaw 有两个互补的记忆层：**长期记忆**（跨会话的事实/偏好）和**会话记忆**（对话内的滚动摘要）。

#### 长期记忆 (`memory.jsonl`)

| 方面         | 细节                                                                              |
| ------------ | --------------------------------------------------------------------------------- |
| **作用域**   | 两级：全局（`~/.baoclaw/memory.jsonl`）和项目级（`<项目>/.baoclaw/memory.jsonl`） |
| **分类**     | `fact`（用户告知的事实）、`preference`（用户偏好）、`decision`（决策记录）        |
| **存储**     | 追加写入 JSONL，每行一个 JSON 对象，文件权限收紧为仅属主（0600）                  |
| **注入方式** | 守护进程启动时加载 → `build_prompt_fragment()` → 追加到系统提示词                 |
| **召回**     | `MemorySearch` 工具 — 对记忆库的关键词搜索，命中的条目获得召回加权                |
| **管理命令** | `/memory add`、`/memory list`、`/memory delete`、`/memory clear`                  |

守护进程启动时，`MemoryStore::load()` 读取全局 `~/.baoclaw/memory.jsonl`，`build_prompt_fragment()` 生成格式化文本块，成为每次对话（per query）都重新渲染并注入的 `append_system_prompt` 的一部分 —— 因此会话中途的 `MemoryTool` 保存无需重启即可生效。

**有界的提示词片段：** 片段不再全量倾倒记忆库 —— 条目按衰减后的重要度排序（`importance × decay_rate^天数`），在字符预算（`memory.prompt_char_budget`，默认 6000）内渲染；表头带有用量仪表（`[3/21 memories · 480/6000 chars]`），装不下的条目通过尾注指向 `MemorySearch` 工具。加载时会丢弃内容完全重复的条目。

**单一写入方：** `MemoryTool` 与 `MemorySearchTool` 共享守护进程的常驻 `MemoryStore` 实例 —— 保存前经过内容校验（`validate_memory_content`，凭据/注入文本无法进入记忆库）、去重（完全相同的重复保存是幂等的），并且无需重启即可反映到提示词片段中。守护进程运行期间手工编辑 `memory.jsonl` 需重启才能生效。

**实时召回：** `MemorySearch` 让模型可以用关键词检索所有未进入常驻片段（或不值得常驻）的记忆；命中按关键词覆盖率 + 重要度评分，且每次返回都会记录一次召回（`recall_count`、`last_recalled_at`、重要度加权）—— 这正是让常用记忆不被"仅按年龄衰减→归档"路径清掉的信号。

**写入规范（MemoryTool）：** `content`（一条陈述句）、`category`，以及可选的 `importance`（0.0–1.0，默认 0.5）—— 重要度决定条目在片段中的排序，并减缓其衰减。

#### 会话记忆 (`session_memory.rs`)

每个会话的滚动摘要，在会话生命周期内持续精炼 —— 就像不断完善的会议纪要。

| 方面         | 细节                                                                           |
| ------------ | ------------------------------------------------------------------------------ |
| **存储位置** | `~/.baoclaw/sessions/{session_id}.memory.md`                                   |
| **首次更新** | **6 条消息**时触发（如果摘要是空的）                                           |
| **刷新间隔** | 每隔 **10 条消息**更新一次                                                     |
| **停滞保护** | 历史长度缩短时（压缩后）重新锚定间隔基线 —— 否则摘要会在会话剩余时间内永久停更 |
| **触发位置** | 工具调用轮次之后，以及纯文本的最终轮次                                         |
| **线程安全** | `std::sync::Mutex` — 可通过 `Arc<SessionMemory>` 安全共享                      |
| **持久化**   | 每次 `update()` 调用都立即写入磁盘                                             |

**时效标注：** 当摘要落后当前历史超过 **10 条消息**时，动态提醒会附加一条
`# Summary Freshness` 注记（"摘要上次更新于 N 条消息之前，之后的工作可能未收录"），
让模型把它当作快照而非当前事实。会话刚恢复时更新时间未知，此时不显示注记
（宁缺毋滥）。

**用途：**

1. **免费压缩** — `session_memory_compact()` 用已有摘要替换旧消息，无需 API 调用（保留最近 10 条）
2. **动态提醒** — 注入到用户消息的 `<system-reminder>` 中，与 git 状态并列；仅在**用户轮次开始时注入一次**，任务执行中的工具结果轮次不会重复注入
3. **会话恢复** — 按**表面（surface）隔离**：会话只恢复自己的转录（`{cwd_hash}-{surface}` 精确匹配优先，其次同表面的最新记录），绝不继承其他表面的历史；从快照恢复时，仅当会话自身的 `.memory.md` 为空才种入摘要，避免陈旧快照覆盖较新的文件

**摘要新鲜度：** 摘要器的输入保留对话的**最近 ~40K 字符**（因此摘要追踪当前工作，而不是冻结在会话开始的几分钟），并且其中每个工具结果块都被裁剪为 400 字符的逐字头部（`[...N chars elided]`）—— 过去原始工具 JSON 会把真正的对话挤出 40K 窗口。摘要本身遵循固定的章节布局（**任务概览 / 当前状态 / 关键发现 / 下一步 / 需保留的上下文**），并附带"逐字复制字面量"规则，确保精确的标识符、路径和错误信息在压缩后依然保留。

---

### 2. 📐 上下文机制

BaoClaw 以 200K token 上下文窗口为目标，采用多层压缩策略和校准后的 token 计数。

#### Token 计数 (`token_counter.rs`)

| 方面             | 细节                                                               |
| ---------------- | ------------------------------------------------------------------ |
| **上下文窗口**   | 200,000 tokens（默认）                                             |
| **自动压缩阈值** | 70% = **140,000 tokens**                                           |
| **分词器**       | `cl100k_base`（GPT-4 分词器，对 Claude 约多算 5-10%）              |
| **计数策略**     | 从 API 响应的 `usage.input_tokens` 校准 → 锚定基线 + tiktoken 增量 |
| **基线持久化**   | `~/.baoclaw/sessions/{id}.baseline.json` — 重启后恢复校准值        |

**预算等级（200K 窗口）：**

| 等级         | 阈值         | 行为                        |
| ------------ | ------------ | --------------------------- |
| **Normal**   | < 140K       | 正常运行                    |
| **Compact**  | ≥ 140K (70%) | 触发预防性压缩              |
| **Warning**  | ≥ 147K       | 记录警告日志                |
| **Blocking** | ≥ 164K       | 必须在下一次 API 调用前压缩 |

#### 5 级压缩层次

从代价最低到最高依次尝试：

```
┌─────────────────────────────────────────────────────────────────┐
│ 第 1 级: micro_compact  (免费，每轮执行)                         │
│   • 清除 > 8192 字符 且 > 24 小时的 tool_result 内容            │
│     （配置项: micro_compact_min_chars / _min_age_secs）          │
│   • 跳过最近 4 条消息（当前轮次）                                 │
│   • 替换文本标注工具名: "[Old tool result cleared —              │
│     Bash output, originally N chars]"                           │
├─────────────────────────────────────────────────────────────────┤
│ 第 2 级: session_memory_compact  (免费，无需 API)                │
│   • 使用已有的 SessionMemory 滚动摘要                           │
│   • 保留最近 10 条消息，前置 CompactBoundary                    │
│   • 预算状态为 Compact/Blocking 时触发                          │
├─────────────────────────────────────────────────────────────────┤
│ 第 3 级: compact_messages  (1 次 API 调用，缓存安全)             │
│   • 保留最近 10 条消息，通过 API 摘要旧消息                     │
│   • 缓存安全分叉: 复用系统提示词 + 旧消息作为 API 消息           │
│     → 在提供商侧复用缓存前缀                                    │
│   • 摘要输入截断到 60,000 字符（约 15K tokens）                  │
│   • 熔断器: 连续失败 3 次后跳过                                  │
├─────────────────────────────────────────────────────────────────┤
│ 第 4 级: reactive_compact  (免费，最后手段)                      │
│   • 按轮次分组消息，丢弃最早的 20%                               │
│   • 保护: ≤ 4 条消息或 ≤ 2 轮时不丢弃                           │
├─────────────────────────────────────────────────────────────────┤
│ 第 5 级: inline compact  (上下文溢出错误时触发)                   │
│   • API 返回 "model_context_window_exceeded" 时触发             │
│   • 保留最近 4 条消息，通过内联 API 调用摘要旧消息              │
│   • 用压缩后的上下文重试查询                                     │
└─────────────────────────────────────────────────────────────────┘
```

#### 会话恢复流程 —— 摘要优先三层策略

```
1. find_latest_session_for_cwd(cwd, session_id)
     → 对 cwd 做 FNV-1a 哈希 → 扫描 ~/.baoclaw/sessions/ 匹配的 .jsonl，
       仅限调用者自己的表面（先精确 id，其次同表面最新）
2. TranscriptWriter::load(session_id) → 读取所有条目
3. SessionMemory::load(session_id) → 检查 .memory.md 是否有预写摘要
4. 三层加载策略（10 分钟 → < 5 秒）:
   Tier 1（最佳）: 有摘要 → 只加载摘要 + 最近 200 条 entries
   Tier 2（小会话）: entries ≤ 400，无摘要 → 安全 rebuild 全部
   Tier 3（兜底）: 大会话无摘要 → 只取最后 200 条 + 警告消息
5. engine.set_messages(messages)
6. engine.load_token_baseline(session_id)   // 恢复校准计数
7. engine.seed_session_memory(&old_summary) // 继承摘要到新 session
```

**后台摘要生成**确保 Tier 1 始终可用：

- 首次在 6 条消息后更新，之后每 10 条消息更新一次（`tokio::spawn`，非阻塞）
- Session 关闭时 heuristic 兜底（如果后台更新从未触发）
- Pre-query compact 安全策略：>500 条消息 → `session_memory_compact`（免费）或 tail-trim（无 API 调用）

---

### 3. 🔄 进化机制

实验性自我进化引擎记录交互数据，并可创建或改进可复用的技能候选。

#### 文件布局

```
~/.baoclaw/evolution/
├── trajectories.jsonl          # 每次交互记录（追加写入）
├── session_summaries.jsonl     # 每次会话关闭的结构化摘要
├── skill_stats.json            # 每个技能的使用追踪
├── pending_review.json         # 跨会话审查 → 下次会话的提示词
├── pending_eval.json           # 自我评估提醒（一次性消费）
└── candidates/
    └── {skill-name}.json       # 自动提取的技能候选
```

#### 关键阈值

| 常量                       | 值           | 用途                                          |
| -------------------------- | ------------ | --------------------------------------------- |
| `SKILL_CREATION_THRESHOLD` | 3 次工具调用 | 自动提取技能候选的最低复杂度                  |
| `SELF_EVAL_INTERVAL`       | 15 个任务    | 触发自我评估提醒                              |
| 审查触发条件               | ≥ 2 轮       | 只有 ≥ 2 轮的会话才生成 `pending_review.json` |
| 候选名称最大长度           | 60 字符      | 从用户提示词 slug 化                          |
| 话题截断                   | 200 字符     | 会话摘要中每个话题                            |

#### 进化生命周期

**交互期间** (`record_trajectory`)：

```
每次用户交互
    ├── 追加轨迹到 trajectories.jsonl
    ├── 递增 task_count
    ├── 如果 tool_count ≥ 3 且结果 = Completed:
    │     → 自动提取 SkillCandidate → 保存到 candidates/{name}.json
    └── 每 15 个任务 → 写入 pending_eval.json（一次性提醒）
```

**会话关闭** (`on_session_close`) — 纯 Rust，无 LLM 调用：

```
最后一个客户端断开
    ├── 从消息历史提取:
    │     用户话题、工具使用频率、错误、已加载技能
    ├── 写入 session_summaries.jsonl
    └── 如果 turn_count ≥ 2:
          写入 pending_review.json（给下次会话的提示词）
```

**系统提示词注入** (`build_prompt_fragment`)：

```
新会话开始
    ├── 检查上次会话的 pending_review.json
    │     → 生成 "Last Session Review" 及自我改进建议
    ├── 检查 pending_eval.json
    │     → 生成 "Self-Evaluation Nudge"
    ├── 列出待处理的技能候选
    └── 全部注入到 append_system_prompt → 系统提示词第 5 层
```

**技能提升** (`promote_skill`)：

```
候选被批准 → 从 candidates/ 移到 ~/.baoclaw/skills/{name}.md
              删除候选文件
              技能在所有后续会话中加载
```

**训练数据导出** (`export_training_data`)：

```
读取所有轨迹 → 创建偏好对
    每对: { prompt, response, rating: chosen/rejected/neutral }
    输出: ~/.baoclaw/evolution/training_export.jsonl
    适用于 DPO/RLHF 微调
```

---

### 4. 📋 系统提示词构建

系统提示词由 5 个有序层组装。顺序对 API 提示词缓存至关重要 —— 稳定的层排在前面：

```
┌──────────────────────────────────────────────────────────┐
│ 第 1 层: 核心系统提示词                                    │
│   • 默认: "You are a helpful AI coding assistant."       │
│   • 可覆盖: config 中的 custom_system_prompt              │
│   • 包含: "显示完整内容"指令                               │
│   • 缓存: cache_control = ephemeral                      │
├──────────────────────────────────────────────────────────┤
│ 第 2 层: 工作目录                                         │
│   • 当前 cwd 路径                                        │
│   • 指示 Agent 输出完整文件内容                            │
├──────────────────────────────────────────────────────────┤
│ 第 3 层: 项目指令                                         │
│   • 来自 BAOCLAW.md（项目根目录或 .baoclaw/）              │
│   • 加载一次，跨轮次缓存                                   │
│   • 同时加载 .baoclaw/rules/*.md（按路径过滤）             │
├──────────────────────────────────────────────────────────┤
│ 第 4 层: 追加系统提示词                                    │
│   • 技能（个人级 ~/.baoclaw/skills/ + 项目级）             │
│   • 长期记忆（事实、偏好、决策）                            │
│   • 进化提示词（待处理审查、技能候选）                       │
├──────────────────────────────────────────────────────────┤
│ 动态 <system-reminder>（在最后一条用户消息中）              │
│   • 不在系统提示词内 — 保持缓存稳定性                      │
│   • Git 状态（分支、修改文件）                              │
│   • 会话记忆（滚动摘要）                                   │
└──────────────────────────────────────────────────────────┘
```

**为什么这样拆分？** 静态层（1-4）标记了 `cache_control: ephemeral`，API 提供商可以缓存前缀。只有动态的 `<system-reminder>` 每轮变化 —— 它被注入到用户消息而非系统提示词中，因此系统提示词缓存保持有效。

**技能和记忆的加载流程：**

1. 守护进程启动时: `load_skills_for_prompt(cwd)` → 发现所有技能 `.md` 文件
2. 守护进程启动时: `MemoryStore::load()` → 读取全局 `~/.baoclaw/memory.jsonl`（项目级存储 API 存在，但目前不加载进提示词）
3. 通过 `build_append_prompt()` 合并 → 成为第 4 层
4. 进化引擎的 `build_prompt_fragment()` 添加待处理审查和技能候选
5. 所有这些在启动时计算一次，跨轮次复用

---

### 5. 🔁 模型退回机制

当主要模型不可用或被限流时，BaoClaw 自动退回到可配置的模型链中的下一个模型。

#### 退回链

```
使用主要模型发请求
    │
    ▼
┌─ 被限流 (429)? ─────── Yes ──→ retry_count < max_retries?
│                                   │              │
│                                  Yes             No
│                                   │              │
│                                   ▼              ▼
│                              指数退避重试     链中有下一个模型?
│                                               │           │
│                                              Yes          No
│                                               │           │
│                                               ▼           ▼
│                                          退回到         已耗尽
│                                          下一个模型     (全部试过)
│                                          (重置计数器)
│
├─ 服务器错误 (5xx)? ──── Yes ──→ server_error_count < 3?
│                                   │              │
│                                  Yes             No
│                                   │              │
│                                   ▼              ▼
│                              退避重试         走退回链
│                              (1s, 2s, 4s)    (同上)
│
├─ 上下文溢出? ──────────── Yes ──→ 先尝试压缩
│                                   │
│                                   ▼
│                              compact_messages() 或 reactive_compact()
│                                   │
│                              用压缩后的上下文重试
│
└─ 成功 ◀──────────────── 返回响应
```

#### 配置

```json
{
  "model": "claude-sonnet-4-20250514",
  "fallback_models": ["claude-3-5-haiku-20241022"],
  "max_retries_per_model": 2
}
```

| 参数                    | 默认值                     | 说明                     |
| ----------------------- | -------------------------- | ------------------------ |
| `model`                 | `claude-sonnet-4-20250514` | 主要模型（每次优先尝试） |
| `fallback_models`       | `[]`                       | 有序的退回模型列表       |
| `max_retries_per_model` | `2`                        | 每个模型退回前的重试次数 |
| 服务器错误最大重试      | `3`                        | 5xx 错误的内置限制       |

#### 错误恢复策略

| 错误类型                   | 策略     | 参数                          |
| -------------------------- | -------- | ----------------------------- |
| IPC 断连                   | 重启进程 | 完整守护进程重启              |
| 状态同步失败               | 全量同步 | 从头重新同步                  |
| API 限流 (429)             | 退避重试 | 3 次尝试，初始延迟 1s         |
| API 服务器错误 (5xx)       | 退避重试 | 3 次尝试，指数退避 (1s→2s→4s) |
| API 认证错误               | 致命     | 无法自动恢复                  |
| API 请求错误（上下文溢出） | 自动压缩 | 压缩 → 重试                   |
| 工具超时                   | 致命     | 报告给用户                    |

**关键行为：**

- 退回控制器对每个新查询**重置**到主要模型（跨轮次无状态）
- **指数退避**防止持续冲击限流端点
- **熔断器**：连续 3 次压缩失败后，禁用自动压缩以避免浪费 API 调用
- 每次 API 调用 **5 分钟超时** — 超时时移除用户消息以保持历史记录干净

---

### 数据流总览

```
用户输入
    │
    ▼
main.rs (加载技能 + 记忆 + 进化 → append_system_prompt)
    │
    ▼
QueryEngine.submit_message_with_attachments()
    ├── Token 预算检查 → 需要时自动压缩
    │     ├── session_memory_compact()  (免费)
    │     └── compact_messages()        (1 次 API 调用，缓存安全)
    │
    └── tokio::spawn(run_query_loop)
          │
          ▼  每轮:
          ├── micro_compact()              (每轮，免费)
          ├── 预算状态检查                  → 可能触发压缩
          ├── build_system_prompt()        → 静态缓存前缀（5 层）
          ├── build_dynamic_reminder()     → 注入到最后一条用户消息
          ├── FallbackController           → 模型选择
          │     ├── 429 → 重试 / 退回
          │     ├── 5xx → 重试 / 退回
          │     └── 上下文溢出 → 压缩 + 重试
          ├── UnifiedClient.stream()       → Anthropic 或 OpenAI
          ├── 工具执行                      → 发出事件
          ├── SessionMemory.should_update() → 间隔到达时更新摘要
          └── TranscriptWriter.append()    → 持久化到 JSONL
                │
                ▼  会话关闭:
          EvolutionEngine.on_session_close()
                ├── 写入 session_summaries.jsonl
                ├── 写入 pending_review.json  (→ 下次会话)
                └── 适当时提取技能
```

## 安装

### 前置条件

- Rust (1.75+) — [rustup.rs](https://rustup.rs)
- Node.js (18+) — [nodejs.org](https://nodejs.org)
- LLM API Key（Anthropic、OpenRouter 或任意 OpenAI 兼容服务）

### Linux / macOS

```bash
git clone https://github.com/baohx/BaoClaw.git
cd BaoClaw
./install.sh
```

### Windows (WSL2)

```powershell
wsl --install
# 在 WSL2 中
git clone https://github.com/baohx/BaoClaw.git
cd BaoClaw
./install.sh
```

### 使用

```bash
export ANTHROPIC_API_KEY=sk-ant-...
baoclaw
```

OpenAI 兼容模式：

```bash
export ANTHROPIC_API_KEY=your-key
export ANTHROPIC_BASE_URL=https://your-provider.com/v1
baoclaw
```

## 配置文件参考

详细的配置文件说明请参考英文版 [Configuration Reference](#configuration-reference) 部分。

简要概览：

| 文件             | 位置               | 说明                                |
| ---------------- | ------------------ | ----------------------------------- |
| `config.json`    | `~/.baoclaw/`      | 主配置（模型、API、Telegram token） |
| `BAOCLAW.md`     | `<项目>/.baoclaw/` | 项目指令，注入系统提示词            |
| `mcp.json`       | 两级都有           | MCP 服务器配置                      |
| `mcp.local.json` | `<项目>/.baoclaw/` | 本地 MCP 覆盖（gitignore）          |
| `memory.jsonl`   | 两级都有           | 记忆存储                            |
| `cron.json`      | `~/.baoclaw/`      | 定时任务                            |
| `skills/*.md`    | 两级都有           | 技能文件                            |
| `todo.json`      | `<项目>/.baoclaw/` | 项目待办                            |
| `evolution/`     | `~/.baoclaw/`      | 进化数据（轨迹、候选 skill）        |
| `sessions/`      | `~/.baoclaw/`      | 会话记录（按项目）                  |

环境变量：

- `ANTHROPIC_API_KEY` — API 密钥（必需）
- `ANTHROPIC_MODEL` — 覆盖配置中的模型
- `ANTHROPIC_BASE_URL` — OpenAI 兼容 API 地址
- `BRAVE_SEARCH_API_KEY` — Web 搜索 API 密钥

## 完整命令列表

| 命令                      | 说明                                          |
| ------------------------- | --------------------------------------------- |
| `/projects`               | 项目管理：list, <id>, new <路径> [描述], desc |
| `/tools`                  | 列出已注册的工具                              |
| `/mcp [refresh [服务器]]` | 列出 MCP 服务器（实时状态）· 刷新/重连        |
| `/skills`                 | 列出已加载的技能                              |
| `/plugins`                | 列出已安装的插件                              |
| `/model [名称]`           | 查看或切换模型                                |
| `/think`                  | 切换扩展思考模式                              |
| `/compact`                | 压缩对话上下文                                |
| `/memory`                 | 长期记忆：list, add, delete, clear            |
| `/cron`                   | 定时任务：add, list, remove, toggle           |
| `/diff`                   | 查看 git diff                                 |
| `/commit <消息>`          | 暂存并提交                                    |
| `/git`                    | 查看 git 状态                                 |
| `/task`                   | 后台任务：run, list, status, stop             |
| `/tasks`                  | `/task` 的完整别名                            |
| `/spec`                   | 规格工作流：list、new、show、status、run      |
| `/search`                 | 搜索对话历史：/search <查询>                  |
| `/export`                 | 导出会话记录为 Markdown                       |
| `/status`                 | 守护进程连接与会话概览                        |
| `/voice`                  | 语音输入（需要 whisper.cpp）                  |
| `/telegram`               | 管理 Telegram 网关                            |
| `/telemetry`              | 遥测：status、on/off、stats、trends、export   |
| `@file.pdf`               | 附加文件进行问答                              |
| `/abort`                  | 取消当前请求（或按 Ctrl+C）                   |
| `/clear`                  | 清屏                                          |
| `/help`                   | 显示所有命令                                  |
| `/quit`                   | 断开连接（守护进程保持运行）                  |
| `/shutdown`               | 停止守护进程                                  |

## 定时任务示例

```
/cron add "每日git总结" "daily 09:00" 总结昨天的git提交
/cron add "依赖检查" "weekly mon 10:00" 检查项目依赖安全更新
/cron add "进化评估" "every 2h" 检查待处理的skill候选并改进
/cron list
/cron toggle abc123
/cron remove abc123
```

## 自我进化：工作原理

```
使用 BaoClaw ──→ 记录交互轨迹
                      │
                      ▼
              复杂任务成功完成？
                 │          │
                是           否
                 │          │
                 ▼          ▼
          提取 skill      (跳过)
          候选
                 │
                 ▼
        每 15 个任务 ──→ 触发自我评估
                 │
                 ▼
        Agent 创建/改进 skill
                 │
                 ▼
        下次会话加载新 skill
                 │
                 ▼
        表现更好 ──→ 循环继续
                 │
                 ▼
        导出轨迹数据 ──→ RLHF/DPO 微调小模型
```

## 许可证

MIT
