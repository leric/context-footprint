# CF 新算法与当前设计的差异分析

本文以 [`design.md`](design.md) 及其当前 Rust 实现为现状基线，以
[`alg.md`](alg.md) 为目标，回答三个问题：

1. 哪些部分可以直接保留；
2. 哪些部分只是数据结构升级；
3. 哪些部分改变了 CF 的定义或遍历语义，必须重写并重新验证。

## 1. 结论

新算法不是对当前 pruning rules 的小幅调整，而是一次以 **measurement
model** 和 **traversal state** 为中心的升级。

可以保留的主要是：

- Extractor → Builder → Query 三层结构；
- `SemanticData`、Program Fact Graph、Type Registry 的总体边界；
- Function / Variable 两类图节点；
- `Call`、`Read`、`Write`、`OverriddenBy`、`Annotates` 五类事实边；
- 在查询期从 incoming edges 推导反向 reasoning relation；
- Builder 的多 pass 主体和现有 extractor 的大部分语义解析能力。

必须改变的核心是：

- 从一个混合 reachable set 和单一 scalar，改成 `CF-out`、`CF-in`、
  overlap 与 union；
- 从 `visited: Set<NodeId>` 改成 reasoning-state 去重和 source-fragment
  计费去重两套机制；
- 从每个节点一个 `context_size` 改成 surface / implementation fragments；
- 从固定的 `PruningParams` + domain functions 改成 relation-aware、
  mode-aware 的 `BoundaryPolicy`；
- 重新定义 caller、shared-state writer/reader、override 的展开方向和停止规则；
- 截断结果从“一个看似完整的数”改成明确的 lower bound。

因此，迁移的主要风险不在 Graph Builder，而在 Solver、Boundary Policy
和 context fragment 数据协议。

## 2. 总体差异矩阵

| 维度 | 当前设计 / 实现 | `alg.md` 目标 | 变化性质 |
|---|---|---|---|
| 测量对象 | 一个混合的可达节点集 `R(u)` | 独立的 `R_out(u)`、`R_in(u)` 及其 union / overlap | **定义变化** |
| 输出指标 | `total_context_size` | `cf_out`、`cf_in`、`cf_total`、`cf_overlap` | **定义变化** |
| context 单位 | Node | `ContextFragment` | **数据模型变化** |
| 节点成本 | 单一 `context_size` | `surface_fragment` + `implementation_fragment` | **数据模型变化** |
| traversal identity | `NodeId` | `(NodeId, Direction, ReasoningMode)` | **算法变化** |
| cost identity | visited node | `ContextFragmentId` | **算法变化** |
| 方向表达 | forward traversal + 两类 reverse exploration | 从 program facts 按 mode 派生 reasoning dependencies | **概念与算法变化** |
| pruning 输入 | source、target、edge kind、固定 params | reasoning mode、relation、source、target、type registry | **接口变化** |
| pruning 输出 | Boundary / Transparent | StopAtSurface / EnterImplementation / Unknown | **接口变化** |
| caller 展开 | 特殊 call-in 分支 | CF-in 的 Usage reasoning | **语义变化** |
| mutable state | reader 找 writers | CF-out reader 找 writers；CF-in writer 找 readers | **语义扩展** |
| override | 主要作为正向 `OverriddenBy` 边 | CF-out dispatch + CF-in parent/child obligations | **语义扩展** |
| external | 固定 boundary heuristic | measurement scope 之外，只计 API surface | 行为相近，**解释变化** |
| truncation | 达到 `max_tokens` 后停止并返回 total | lower bound + `truncated` + unresolved states | **结果语义变化** |
| policy provenance | academic / strict params | `boundary_policy_id` | **可复现性增强** |
| graph quality | 隐含在 extractor / builder | 显式 `graph_coverage` 和 construction precision | **可解释性增强** |

## 3. 保持不变或可复用的部分

### 3.1 三层架构

两个文档都使用：

```text
Extractor → Semantic Data → Graph Builder → Program Graph → CF Query
```

新算法进一步明确：Builder 只描述代码中存在的 program facts，Query
负责把事实投影为 reasoning dependencies。当前把 call-in 和 shared-state
反向探索放在 Solver，而不物化 `CallIn` / `SharedStateWrite` 边，方向是正确的。

### 3.2 Program Fact Graph

以下部分无需结构性重写：

- 图节点仍只有 Function / Variable；
- Type 仍存入独立 Type Registry；
- 五种 `EdgeKind` 不变；
- `OverriddenBy` 仍是 Parent Method → Child Method；
- incoming edge 查询仍是动态推导 caller、writer、reader 的基础。

现有 `ContextGraph` 及 petgraph 封装可以继续使用。

### 3.3 Builder passes

当前多 pass 流程与目标基本一致：

1. Node allocation + Type Registry；
2. static forward edges；
3. type propagation；
4. `OverriddenBy` + unresolved call recovery。

主要新增工作是生成 fragments、保留 policy 所需的原始 contract 信息，以及
提供 coverage / unresolved 信息，而不是推翻 edge building。

### 3.4 Semantic facts

`SemanticData` 的 definitions、references、type information、symbol hierarchy
仍然适用。现有 extractor 的 symbol resolution、receiver type recovery、
inheritance 和 external symbol 处理可以复用。

但“JSON 外形大体保留”不等于“不需要 schema 升级”；surface /
implementation 的准确切分目前缺少足够的 source range，见 5.1。

## 4. CF 定义与 Solver 的根本变化

### 4.1 从单一 CF 改为 CF-in / CF-out

当前实现只运行一次 BFS，把 outgoing dependencies、callers 和 shared-state
writers 混在同一个 `reachable_set` 中：

```text
CF(u) = Σ size(v), v ∈ R(u)
```

这会丢失关系的解释：

- 一个 caller 是 usage obligation，还是因为其他路径成为 dependency；
- 一个节点来自 CF-in 还是 CF-out；
- 同一个 fragment 是否被两侧同时需要；
- 重构降低的是依赖深度还是使用约束。

新算法要求两个逻辑上独立的 traversal：

```text
R_out(u): dependencies needed to understand u
R_in(u):  usages and obligations constraining u
CF(u) = Size(R_out(u) ∪ R_in(u))
```

迁移后不能再从旧 `reachable_set` 推导出准确的 in/out 分解；必须在 traversal
时保留 direction。

### 4.2 visited node 不再足够

当前 `solver.rs` 使用 `HashSet<NodeIndex>` 或 `Vec<bool>` 去重。节点第一次被
访问后，其他 reasoning path 不再处理。

新算法中，同一个函数可能分别以以下身份到达：

- callee behavior；
- caller usage evidence；
- mutable-state writer behavior；
- override contract evidence。

这些身份需要派生不同的下一步 relation。正确的状态 identity 至少为：

```text
(NodeId, Direction, ReasoningMode)
```

同时，状态可以多次访问同一 artifact，但 source fragment 只能计费一次。因此
需要分开：

```text
visited_states
out_fragments
in_fragments
```

总 CF 最后由 fragment set union 计算，而不是在入队时累加 node size。

### 4.3 当前 reverse node 的“立即停止”必须拆开

当前实现对 `CallIn` 和 `SharedStateWrite` 使用同一个规则：计入该节点后，
不再从它展开任何关系。

目标算法对两者的要求不同：

- Usage mode 到达 caller 时，不展开 caller 的 callees，但如果 caller
  contract 仍不完整，要继续访问 caller 的 callers；
- 通过 mutable state 找到 writer 后，writer 进入 Behavior mode，需要继续
 展开其行为依赖。

因此，`ReachedVia::{CallIn, SharedStateWrite}` 不能继续充当“是否展开”的
代理。必须由 reasoning mode 决定允许派生哪些 relation。

### 4.4 CF-in 的 caller chain

当前实现只收集一层 caller。新算法要求：

1. 当前 function contract 足够：停止，不派生 callers；
2. contract 不足：caller 的 surface + implementation 成为 usage evidence；
3. caller contract 足够：在 caller 停止；
4. caller contract 仍不足：继续沿 incoming Call 到更上层 caller；
5. Usage mode 不进入 caller 的普通 outgoing callees。

这不是把现有 `CallIn` 节点重新入普通 BFS；需要一个只允许沿 usage
obligation 继续传播的 mode。

### 4.5 shared mutable state 变为双向规则

当前设计只覆盖：

```text
reader --Read--> state <--Write-- writer
```

即从 reader 的 dependency side 找 writer。

新算法增加并区分：

- **CF-out(reader)**：计入 state，并以 Behavior mode 展开 writers；
- **CF-out(writer)**：对 `Write` 只计入 state surface，不搜索其他
  writers/readers；
- **CF-in(writer)**：如果 writer contract 不能说明 side effect，则计入
  state 及 readers 的 surface + implementation，但不继续从 reader 展开。

当前“Write 永远 Transparent”不再是统一正确的规则。它把 relation 的含义
交给 target node 类型判断，而新算法要求由 direction + reasoning relation
共同决定。

### 4.6 override obligations

当前实现主要把 `OverriddenBy` 当成普通 forward edge：从 parent method
走向 child method，再对 target 套用普通 function pruning。

新算法至少需要三种语义：

1. **CF-out dynamic dispatch**：parent/interface contract 不可靠时，展开
   concrete implementations；
2. **CF-in child → parent obligation**：当 `u` override parent `p` 时，
   `surface(p)` 总是计入；必要时从 `p` 继续找 usage evidence；
3. **CF-in parent → children evidence**：当 `u` 被 children override 且
   `u` contract 不足时，children 的 surface + implementation 计入，但不再
   从 child 向上展开。

图上的 `OverriddenBy` 边可以保留，但 Query 必须同时使用 outgoing 和
incoming 方向派生不同 reasoning relations。

## 5. Context fragment 数据模型

### 5.1 当前 Semantic Data 无法可靠切分 surface / implementation

当前 `SymbolDefinition` 只有一个覆盖完整 definition 的 `span`。对于普通函数，
该 span 包含 signature 和整个 body。Builder 是语言无关的，不能可靠地从这一个
span 中解析出 Python、Rust、TypeScript 等语言各自的 signature/body 边界。

目标模型却要求：

```text
surface_fragment
implementation_fragment
```

因此至少要选择一种方案：

#### 方案 A：Extractor 提供 fragment spans（推荐）

为 definition 增加语言无关的可选 ranges，例如：

```text
surface_spans: Vec<SourceSpan>
implementation_spans: Vec<SourceSpan>
```

Extractor 负责语言语法层面的切分，Builder 只读取和计数。这最符合现有
hexagonal boundary。

#### 方案 B：surface 使用 normalized text

Builder 根据 parameter / return / modifier / documentation metadata 生成
normalized surface text，implementation 仍取 source body。

这便于跨语言归一化，但它测量的不再是 agent 实际会读取的原始 source
fragment，而且 body 仍需要一个能排除 signature/docs 的 span。

#### 方案 C：Builder 内解析源码

不推荐。它会把语言知识引入 language-agnostic Builder。

`alg.md` 当前写了保留 Semantic Data shape，同时要求 Builder 生成两个
fragments；这两项要求之间还缺一个明确的数据来源。实施前应补全该 contract。

### 5.2 documentation 的计费语义发生变化

当前 size adapter 会从代码片段中删除 documentation 和常见 comment lines；
`doc_score` 只影响 pruning，不计入 node context size。

新算法把 documentation 明确列为 surface 的一部分。按该定义，一个完善的
boundary：

- 需要支付 signature + documentation 的 surface 成本；
- 通过这个成本避免支付 implementation 成本。

因此 size function 不能再无条件剔除 docs。是否提供一个
“normalized-no-comments”实验性 size function，可以作为独立 extension point，
但默认实现应与 fragment 定义一致。

### 5.3 Type Registry 也需要 fragments

当前 `TypeInfo` 只有 `context_size` 和 `doc_score`。目标模型要求 type
`surface_fragment` 和 documentation，因为 type contract 可能是：

- signature completeness 的依据；
- abstract factory 返回 boundary 的依据；
- agent 实际需要加载的类型上下文。

还需明确：何时把 type surface 加入 reached fragments。目前 type registry
主要参与 policy 判断，其 `context_size` 并未系统地进入 CF。

### 5.4 fragment identity

`ContextFragmentId` 必须稳定且能在 in/out 两侧去重。推荐 identity 至少包含：

```text
(symbol_id, fragment_kind, source_revision_or_content_hash)
```

不能只使用 span：文件变化后同一 span 可能代表不同内容；也不能只使用
content hash：两个不同位置的相同文本可能是两个不同的 reasoning artifact。

## 6. Boundary Policy 的变化

### 6.1 当前 policy 是固定算法的一部分

当前 `CfSolver` 持有 `PruningParams`，直接调用：

- `evaluate_forward(...)`；
- `should_explore_callers(...)`。

policy 只返回 Boundary / Transparent，而且大量行为隐式依赖 edge direction
和 `ReachedVia`。

新接口要求 policy 看到：

```text
reasoning_mode
relation
source
target
type_registry
```

并返回：

```text
StopAtSurface
EnterImplementation
Unknown
```

`Unknown` 默认 conservative 地进入 implementation。这使“CF 的遍历定义”
与“如何评估 contract reliability”分离。

### 6.2 当前实现中存在未进入 design.md 的 heuristics

除 `design.md` 描述的 signature + docs、interface、abstract factory 外，
代码中还有：

- DI-wired complete function 直接作为 boundary；
- constructor 不做 caller exploration；
- tokens-per-caller utility exception；
- side-effect-free call graph exception；
- high-freedom parameter 特殊判断；
- zero-size stub source boundary。

这些规则既不完整存在于 `design.md`，也没有被 `alg.md` 采纳为 CF 定义。
迁移时不应悄悄照搬进 Solver。应逐条归类为：

1. concrete `BoundaryPolicy` 的 heuristic；
2. graph / extractor 数据修复；
3. measurement scope 规则；
4. 应删除的旧 workaround。

尤其是 utility ratio 和 purity exception，会直接跳过 usage evidence，属于可能
改变测量结果的 heuristic，必须带 policy ID 并通过实验验证。

### 6.3 ADR 冲突

当前 ADR-003 明确规定“不使用 policy trait，pruning 完全在 domain 中”。
`alg.md` 则把 Boundary Policy 定义为显式 extension point。

目标实现如果以 `alg.md` 为准，需要先替换或 supersede ADR-003；否则架构规则
会与实现目标冲突。

## 7. External dependency 与 measurement scope

运行行为表面上相同：external node 计入后停止。

解释发生了重要变化：

- 当前：第三方 API 被假设为可靠 abstraction；
- 新算法：external implementation 不在 repository-local measurement universe
  中，与其质量无关。

实现上需要：

- external API surface fragment，而不是整个 external node 的笼统 size；
- 可标识的 measurement scope；
- workspace / monorepo package 是否 internal 的配置；
- 在输出中记录 scope，保证跨版本实验可复现。

## 8. Truncation 和结果可解释性

当前 `max_tokens` 达到阈值后停止，但 `CfResult` 没有 `truncated` 标志，也不记录
尚未处理的状态。调用者可能把 partial total 当成真实 CF。

目标结果必须表达：

```text
CF >= lower_bound
truncated = true
unresolved_states = [...]
```

此外，新结果需要至少报告：

- in/out fragment sets；
- boundary stops；
- unresolved states；
- graph coverage；
- boundary policy ID；
- size function ID；
- measurement scope ID（建议补充）。

当前的 node order、BFS layers 和 witness paths 仍可作为诊断信息保留，但不能
替代上述 metric provenance。

## 9. `alg.md` 中实施前需要澄清的两个问题

### 9.1 `paid_fragments` 与 in/out 分解

伪代码把同一个 `paid_fragments` 依次传入 out traversal 和 in traversal，同时
又要求分别计算 `out_result.fragments`、`in_result.fragments` 和 overlap。

实现时不应让全局 paid set 阻止第二个 traversal 把 overlap fragment 记录到
自己的 reached set。推荐规则是：

```text
out_fragments = independent set
in_fragments  = independent set
cf_total      = size(out_fragments ∪ in_fragments)
```

“不重复计费”只应用于集合求和，不应用于是否记录某一侧到达了 fragment。

### 9.2 起始节点的 CF-in 展开条件

目标文档说明 reliable contract 会停止 CF-in，又在 solver pseudocode 中直接以
`NeedContractEvidence` 启动。实现需要明确 relation derivation 的第一步：

- 起始节点 surface + implementation 总是进入两侧共同基点；
- policy 只决定是否从起始节点派生 caller / override / reader obligations；
- 不能因为起始节点 contract 可靠而省略起始 implementation，否则 CF 不再测量
  “理解该 unit 本身”的上下文。

这条规则应写成 golden test，避免 boundary decision 错误作用于 target unit。

## 10. 建议的迁移顺序

### 阶段 0：冻结目标规范

1. supersede ADR-003；
2. 明确 fragment spans 的 Semantic Data contract；
3. 修正 `paid_fragments` 伪代码；
4. 明确 target unit、type surface、docs 的计费规则；
5. 为所有 reasoning relation 建立一张 mode transition table。

### 阶段 1：升级 Graph Schema，不改变 edges

1. 引入 `ContextFragment` / `ContextFragmentId`；
2. Node 拆分 surface / implementation；
3. TypeInfo 改为 type surface fragment；
4. 增加 `owner_type` 或提供等价的稳定查询；
5. Builder 生成 fragments；
6. 保留五种现有 `EdgeKind`。

### 阶段 2：重写 Solver

1. 引入 `Direction`、`ReasoningMode`、`TraversalState`；
2. 独立运行 out / in traversal；
3. 基于 state 派生 relation；
4. 用 fragment sets 计算 in/out/overlap/total；
5. 正确实现 caller chain、writer behavior、readers 和 override obligations；
6. 实现 truncation lower bound。

### 阶段 3：抽取 Boundary Policy

1. 定义 trait 和三态 decision；
2. 先实现一个与目标文档最小规则一致的 syntactic policy；
3. 再决定哪些旧 heuristics 进入 legacy / experimental policy；
4. 输出稳定的 policy ID 和参数。

### 阶段 4：升级 Extractor 和 Builder coverage

1. 输出 surface / implementation ranges；
2. 补全 external API surface；
3. 记录 unresolved references 和恢复方式；
4. 生成 coverage 信息。

### 阶段 5：应用层和实验接口

1. DTO / CLI / MCP 返回新 `CfResult`；
2. 支持 fragment-level explain output；
3. 保留兼容字段时，明确旧 `total_context_size` 是
   `cf_total` alias，而不是旧算法结果；
4. 为 Hermes 实验输出稳定 JSON。

## 11. 最小验收测试集

在接真实 extractor 前，至少用手写 graph 覆盖：

1. reliable callee：只支付 callee surface；
2. leaky callee：支付 surface + implementation 并展开 dependencies；
3. reliable target：CF-in 不展开 callers；
4. under-specified target：递归 caller chain，但不展开 caller 的 callees；
5. mutable read：writer 以 Behavior mode 继续展开；
6. mutable write：CF-out 不找 readers，CF-in 找 readers；
7. child override：parent surface 进入 CF-in；
8. leaky parent：children implementation 进入 evidence；
9. 同一 node 以不同 mode 到达时两种 obligation 都执行；
10. 同一 fragment 同时在 in/out 到达时 overlap 正确且 total 只计一次；
11. cycle 不死循环；
12. external 只计 surface；
13. truncated result 是 lower bound 并包含 unresolved states；
14. docs 增加 surface cost，但可靠 boundary 避免 implementation cost。

这些测试通过后，才适合用真实 Python extractor 和 Hermes 仓库验证算法，而不是
同时调试 traversal semantics 与 symbol resolution。
