# Context Footprint（CF）算法设计

## 0. 定义与边界

Context Footprint（CF）用于测量代码单元的 **structural context depth**：为了充分理解一个代码单元本身，需要加载多少与其结构依赖相关的程序上下文。

CF 只对应 CMP 的 **Depth** 轴，不处理 Breadth。

- **Depth 是 artifact-relative 的结构属性。** 对于给定代码版本、给定静态分析规则，一个代码单元的 Depth 不依赖具体修改场景。
- **Breadth 是 modification-relative 的任务属性。** 它取决于一次具体修改需要同时获得哪些 modification closure 成员，因此不属于 CF 的测量范围。

CF 分为两个方向：

> **CF-out measures the context required to understand what the unit depends on.**
>
> **CF-in measures the context required to understand what depends on, or constrains, the unit.**
>
> **CF measures the structural depth of a code unit by combining both sides.**

直观上：

```text
callers / usages / inherited obligations
                ↓
              CF_in
                ↓
             [ unit ]
                ↓
              CF_out
                ↓
callees / state / decorators / implementations
```

其中：

- `CF_out(u)`：为了理解 `u` 所依赖的行为、状态和实现，需要向外加载的上下文。
- `CF_in(u)`：为了理解 `u` 被外部如何使用、依赖或约束，需要向内加载的上下文。
- `CF(u)`：两侧 footprint 的合并结果。

概念上可以把总 CF 看成 `CF_in + CF_out`。实现上，同一 source fragment 可能同时从两个方向被访问，因此必须去重：

\[
CF(u)=Size(R_{in}(u) \cup R_{out}(u))
\]

其中 `R_in(u)` 与 `R_out(u)` 是两侧实际需要加载的 context fragment 集合。

### 0.1 CF 不是什么

CF **不是代码质量的绝对评分**。

一个 1000 行、几乎没有外部依赖的复杂算法，与一个自身只有 50 行、但必须再读取 950 行依赖才能理解的函数，可能具有相近的 CF。CF 对这两者只做一个陈述：理解它们需要加载的程序上下文规模相近。它不进一步判断哪一个设计更差。

高 CF 既可能来自设计造成的 accidental complexity，也可能来自问题本身的 essential complexity。孤立的绝对值无法区分二者。

因此 CF 最强的设计解释是 **controlled comparison**：

> 当两个设计实现同一 responsibility / equivalent behavior 时，较低的 CF 表示该设计使理解这一责任所需的上下文更少。

典型场景是重构前后比较：

```text
Before refactoring: CF = 4200
After refactoring:  CF = 1300
```

如果行为和责任边界等价，则可以说这次设计变换显著压缩了 structural context depth。

### 0.2 非目标

CF 不试图测量：

- modification breadth / modification closure；
- cyclomatic complexity、nesting 等单元内部复杂度；
- 运行时调用频率；
- 业务重要性、正确性、安全性或性能；
- 一个设计的绝对“好坏”。

这些可以作为独立指标与 CF 联合分析，但不进入 CF 的定义。

---

## 1. Architecture Overview

CF 的实现保留三层架构，通过两个稳定的数据协议隔离复杂度：

```text
Semantic Data Schema              Graph Schema
       ↓                              ↓
[Part 3: Extractor]  →  [Part 2: Graph Builder]  →  [Part 1: CF Query]
 (language-specific)      (language-agnostic)         (metric / traversal)
```

- **Part 3: Extractor**：从语言特定 AST / LSP / compiler information 中抽取 program facts。
- **Part 2: Graph Builder**：把 program facts 构造成语言无关的结构图与类型注册表。
- **Part 1: CF Query**：把结构图投影为 reasoning dependencies，并根据 boundary policy 计算 `CF_in`、`CF_out` 与总 CF。

两个 Schema：

- **Semantic Data Schema**：Part 3 → Part 2，描述语言无关的定义、引用、类型和符号关系。
- **Graph Schema**：Part 2 → Part 1，描述 CF 计算需要的 program fact graph、节点 surface / implementation 信息与 Type Registry。

一个重要原则：

> **Graph Builder 只负责描述代码中存在什么关系；CF Query 决定哪些关系在 reasoning 时需要被展开。**

这样可以避免把 metric policy 混进语言解析或图构建阶段。

---

# Part 1: CF Query

## 2. Program Fact Graph 与 Reasoning Dependency

### 2.1 Program Fact Graph

底层代码库仍建模为有向图：

\[
G_p=(V,E)
\]

节点 `V` 只包含：

- **Function / Method**
- **Variable**

类型定义不作为普通 traversal node，而存放在独立 **Type Registry** 中。

Program Fact Graph 的边描述代码中真实存在的结构关系，而不是直接声明“理解 A 一定需要读 B”。

| Edge Kind | Direction | Semantics |
|---|---|---|
| `Call` | Function → Function | 函数可能调用另一个函数 |
| `Read` | Function → Variable | 函数读取变量 |
| `Write` | Function → Variable | 函数写入变量 |
| `OverriddenBy` | Parent Method → Child Method | 方法覆盖 / interface implementation |
| `Annotates` | Decorated → Decorator | 装饰器改变被装饰对象行为 |

这些边是 **program facts**。

### 2.2 Reasoning Dependency

CF Solver 不直接把每条 program edge 当成必须遍历的 context edge。

CF 使用一个更高层的概念：

> **Reasoning Dependency：为了充分理解当前 reasoning obligation，可能需要加载另一个 artifact 的关系。**

Reasoning Dependency 可以沿 program edge 正向，也可以由 program facts 推导出反向关系。

例如：

```text
caller --Call--> callee
```

对于 `CF_out(caller)`：

```text
caller --needs behavior of--> callee
```

而：

```text
writer --Write--> shared_state
reader --Read----> shared_state
```

对于 `CF_out(reader)`，理解 `shared_state` 当前可能具有的值需要由 `Read + incoming Write` 推导出：

```text
reader
  → shared_state
  → writer_1
  → writer_2
  → ...
```

同样，对于一个 under-specified function，理解其外部使用约束可能需要从 incoming `Call` 推导 `CF_in`：

```text
function
  → caller_1
  → caller_2
  → ...
```

因此原来所谓的 “forward traversal + reverse exploration” 不再被视为两套不同算法；它们统一为：

> **根据当前 reasoning mode，从 Program Fact Graph 派生下一步 Reasoning Dependencies。**

Reasoning Graph 可以按需生成，不需要预先 materialize。

---

## 3. CF-out：Dependency Footprint

`CF_out(u)` 回答：

> 为了理解 `u` 所依赖的行为和状态，我还需要加载什么？

典型 reasoning relations：

### 3.1 Function Call

对于：

```text
u --Call--> v
```

Solver 首先加载 `v` 的 boundary surface。

- 如果 boundary surface 已足以支持 local reasoning：停止；
- 如果不足：进入 `v` implementation，并继续展开 `v` 的 reasoning dependencies。

### 3.2 Immutable / Constant State

对于：

```text
u --Read--> S
```

如果 `S` 是 immutable / constant：

- 加载 `S` 的 declaration / type / initializer 等必要 surface；
- 不需要继续搜索 writers。

### 3.3 Mutable Shared State

如果 `S` 是 mutable：

理解 `u` 读取到的状态可能需要知道谁能写入 `S`。

因此从：

```text
writer_i --Write--> S
```

派生 reasoning relation：

```text
S --value provenance--> writer_i
```

随后 writer 以 `Behavior` mode 继续展开（规则见 7.2，决策见 7.3 D2）。

这仍然属于 `CF_out(u)`：虽然底层 program edge 是反向访问的，但这些 writers 是解释 `u` 所依赖状态所必需的 context。

对于 `u --Write--> S`，Out 方向只需要 `S` 的 surface（声明、类型、可变性）。写入本身不要求知道其他 writer 或 reader；那是 CF-in 的问题（见 4.4）。

### 3.4 Decorators / Hooks

对于：

```text
u --Annotates--> decorator
```

如果 decorator 会影响 `u` 的行为，则属于 `CF_out(u)`。

同样适用于 framework hook、middleware、descriptor 等可被 Extractor / Builder 明确建模的机制。

### 3.5 Dynamic Dispatch / OverriddenBy

如果调用目标是 abstract/interface method：

- 若 abstraction boundary 足够可靠，则在 interface surface 停止；
- 若不足，则通过 `OverriddenBy` 关系展开可能的 concrete implementations。

这使 CF 能区分：

```text
强 contract 的 polymorphism
```

与：

```text
必须阅读所有实现才能知道真实行为的 nominal abstraction
```

---

## 4. CF-in：Obligation / Usage Footprint

`CF_in(u)` 回答：

> 为了理解外部如何使用、依赖或约束 `u`，我还需要加载什么？

最典型的是 incoming Call / usage relation。

### 4.1 Reliable Contract Stops CF-in

如果 `u` 的 contract 已经完整表达：

- accepted inputs；
- outputs；
- relevant preconditions；
- failure / side-effect semantics；
- invariants or obligations；

那么实现者可以从 boundary 本身理解外部允许依赖什么，不需要扫描 callers。

此时 `CF_in(u)` 只包含 `u` 自身（surface + implementation，见 9.1）以及 `u` 所覆盖的 parent method 的 surface（4.3）——不派生任何 caller。

### 4.2 Under-specified Unit Expands to Callers

如果 `u` 的 specification 不足，则 caller usage 可能成为理解真实约束的必要 evidence。

从：

```text
caller --Call--> u
```

派生：

```text
u --usage evidence--> caller
```

每个 caller `c` 的 surface 与 implementation 都计入 `CF_in(u)`：call site 位于 `c` 的 body 中，只看签名无法知道 `c` 传了什么、如何使用返回值。

是否继续向上（`c` 的 callers）由 `c` 自身的 contract 决定：如果 `c` 的签名和文档已经完整说明了 `c` 接受什么输入，那么 `c` 传给 `u` 的值已经被 `c` 的 contract 约束，无需再看 `c` 的 callers；否则 `c` 很可能只是把自己未标注的参数透传给 `u`，`c` 的 callers 成为进一步 evidence。这与 3.1 中 Call 边的判断是同一个 policy 问题（“这个 function 的 contract 是否足够”），只是被问的对象从 target 换成了 source。

因此 CF-in 的终止条件是明确的：`u` 自身 contract 足够（不派生任何 caller）；或某个 caller 的 contract 足够（在该 caller 停止）；或所有可解析的 caller 链都已到达没有 caller 的根。

> **注意**：Usage mode 下到达 caller `c` 时，**不**展开 `c` 的 callee（即不计算 `c` 的 CF-out）。`c` 的其他依赖与 `u` 的 obligation 无关。这是与旧算法的一个显式差异：旧算法把 caller 当普通节点入队，会连带展开其全部 forward 依赖。

### 4.3 Override / Subtype Obligations

对于 parent/interface method，子类 override 也可能构成对 contract 的现实解释或约束。

`OverriddenBy` 在 CF-in 中有两个方向的含义：

- **`u` 覆盖了 parent `p`**（`p --OverriddenBy--> u`）：`p` 的 contract 是 `u` 必须遵守的 obligation，`surface(p)` 总是计入 `CF_in(u)`。此外，通过 `p` 发起的调用在运行时可能到达 `u`，因此当 `u` 自身 contract 不足时，`p` 的 callers 也是 `u` 的 usage evidence：以 `Usage` mode 继续展开 `p`。
- **`u` 被子类覆盖**（`u --OverriddenBy--> u_i`）：当 `u` 的 contract 不足时，子类实现是对“这个方法族被期望做什么”的现实解释。`surface(u_i) + implementation(u_i)` 计入 `CF_in(u)`，但不再从 `u_i` 继续向上（见 7.3 D4）。

### 4.4 Shared-State Readers as Obligations

3.3 的对称情形：如果 `u` 写入可变共享状态 `S`，那么 `S` 的 readers 依赖 `u` 写入的值。修改 `u` 的写入语义时，需要知道谁会观察到这个变化。

从：

```text
u --Write--> S <--Read-- reader_i
```

派生：

```text
u --observed by--> reader_i
```

当 `u` 的 contract 不足以说明其 side effect 时，`surface(S)` 与每个 reader 的 `surface + implementation` 计入 `CF_in(u)`，不再从 reader 继续展开（见 7.3 D5）。

---

## 5. Boundary Policy：把 Metric 与 Heuristic 分离

CF 的核心 metric 不负责回答：

> “这个 interface 的文档到底够不够好？”

这属于 **BoundaryPolicy**。

CF 只要求 BoundaryPolicy 回答一个问题：

> **这个 artifact 的 contract（surface）是否足以支持当前 reasoning mode 下的 local reasoning？**

建议接口：

```text
enum BoundaryDecision {
    Boundary,      // contract 足够，不再展开
    Transparent,   // contract 不足，继续展开
    Unknown,       // 无法可靠判断
}

BoundaryPolicy.evaluate(
    mode,          // 当前 reasoning mode（见 §7）
    relation,      // 派生出的 reasoning relation 种类
    subject,       // 被判断 contract 的 artifact
    context,       // relation 的另一端（仅作参考）
    type_registry,
) -> BoundaryDecision
```

其中：

- `Boundary`：subject 的 surface 足够，不再从这条 relation 继续展开；
- `Transparent`：subject 的 surface 不足，按 7.2 的规则继续展开；
- `Unknown`：无法可靠判断。Solver 按 conservative default 处理为 `Transparent`，并把该 relation 记入 `unresolved`。

两点重要约定：

1. **决策决定“是否继续”，不决定“支付什么”。** 每种 relation 在遇到 target 时支付的内容由 7.2 的规则表固定（Out 方向通常是 surface，Usage 方向是 surface + implementation，因为 call site 在 body 里）。Policy 只影响是否从该 target 派生下一批 relation。
2. **subject 因 mode 而异。** Out 方向（`Behavior` / `Provenance`）判断的是 **target** 的 contract（“我依赖的东西说清楚了吗”）；In 方向（`Usage`）判断的是 **当前节点自身** 的 contract（“我自己说清楚了吗，还是得看别人怎么用我”）。这就是旧算法中“Core Asymmetry”的来源：两个方向问的是同一个问题，只是问的对象不同。

部分判断由 program fact 直接决定，不经过 policy（例如 `Read` 一个 `Const`/`Immutable` 变量总是 `Boundary`，external target 总是停止）。这些在 7.2 的表中标为 `fact`。

### 5.1 BoundaryPolicy 的实现可以变化

一种简单实现可以使用 syntactic heuristic（即当前代码中的 `PruningParams`）：

```text
complete signature + doc_score >= threshold -> Boundary
interface method with complete signature + doc -> Boundary
abstract factory (returns documented abstract type) -> Boundary
otherwise -> Transparent
```

另一种可以增加：

- precise types；
- pre/postcondition markers；
- immutability；
- Result / Option / algebraic data types；
- protocol / interface semantics；
- tests or executable contracts；
- LLM-based contract evaluation；
- agent-trace-calibrated boundary reliability。

这些都只是 **BoundaryPolicy 的不同实现**，不是 CF 定义本身。

这解决了旧算法里最大的概念混杂：

```text
“CF 是什么”
```

与：

```text
“我们当前如何猜测一个 boundary 是否可靠”
```

必须分开。

---

## 6. Context Size：Surface 与 Implementation

旧模型通常把一个 node 只有一个 `context_size`。这会把“到达 boundary”与“进入 implementation”混在一起。

新版应明确拆分：

```text
surface_size(v)
implementation_size(v)
```

### 6.1 Surface

`surface(v)` 是在不进入内部实现的情况下，为理解该 artifact contract 需要加载的内容，例如：

对于 Function：

- name；
- visibility；
- parameter names and types；
- return types；
- decorators / modifiers relevant to contract；
- doc / explicit contract；
- exposed error / effect information。

对于 Variable：

- name；
- type；
- mutability；
- declaration-level documentation；
- initializer（如果它本身定义该值的完整语义）。

#### 6.1.1 Type Surface 随签名一起支付

签名中出现的类型是 contract 的一部分。`def f(cfg: Config) -> Result` 看起来很小，但如果 `Config` 是一个 80 字段的 dataclass，理解 `f` 的 contract 就需要理解 `Config`。如果不计这部分，boundary 可以把成本藏进类型里，CF 会系统性偏低。

规则：

> **支付 `surface(v)` 时，同时支付 `v` 签名（参数、返回值、变量类型）中引用的每个 repository-local 类型的 `TypeInfo.surface_fragment`。**

- 类型的 surface 是它的声明、字段 / 方法签名与文档，不含方法 body（方法是独立的 Function node，只在被调用时才进入 traversal）。
- 类型不是 traversal node，从 type surface 不再派生任何 relation；它仅作为 context fragment 计费。
- 类型参数 / 泛型实例中引用的类型递归适用同一规则（仍由 `paid` 去重）。
- External 类型与 external symbol 同样处理：支付 analyzer 可见的 surface（通常仅名字），不进入。
- Abstract type 的 implementors **不**因为出现在签名中而被支付；只有当对其方法的调用经 `OverriddenBy` 展开时才进入（3.5）。

下文中 `pay_surface(v)` 一律指“`surface(v)` + 其签名引用的 type surfaces”。这是一个对 token 数影响很大的决定，见 7.3 D1。

### 6.2 Implementation

`implementation(v)` 是 surface 之外的内部行为内容，例如：

- function body；
- internal branches；
- concrete algorithm；
- hidden state access；
- nested implementation details。

因此 traversal cost 为：

```text
Boundary:
    pay surface_size(v)
    stop

Transparent:
    pay surface_size(v)
    pay implementation_size(v)
    expand reasoning dependencies
```

这直接表达了 abstraction 的价值：

> 一个 boundary 的价值不是让 dependency 不存在，而是让 implementation 不再进入 required context。

### 6.3 Size Function

默认可以使用：

- LLM tokenizer token count；

也允许替换为：

- lexical token count；
- AST node count；
- normalized token count；
- 其他稳定的信息量近似。

Size Function 是 CF 的显式 extension point，不改变 metric 的结构定义。

---

## 7. Traversal State 与 Transition Rules

旧算法使用：

```text
visited: Set<NodeId>
```

但如果 pruning / expansion 依赖“以什么 reasoning mode 到达这个 node”，只记录 NodeId 不够。

例如同一个 function：

```text
第一次作为 callee 到达
→ 只需要判断 implementation dependency

第二次作为 usage evidence 到达
→ 需要检查 incoming callers / obligations
```

如果第一次访问后就把整个 node 标记为 visited，第二种 reasoning obligation 会被错误跳过。

因此 traversal state 定义为：

```text
TraversalState {
    node_id,
    mode,            // Behavior | Provenance | Usage
}
```

CF 只需要三种 reasoning mode：

| Mode | 适用节点 | 回答的问题 | 典型来源 |
|---|---|---|---|
| `Behavior` | Function | 这个函数做什么？ | Out 方向起点；Transparent 的 callee / decorator / overrider / writer |
| `Provenance` | Variable | 这个可变状态可能取什么值？ | `Read` 一个 mutable variable |
| `Usage` | Function | 外部如何使用 / 约束这个函数？ | In 方向起点；contract 不足的 caller / parent method |

为什么不需要更多：

- “是否支付 implementation”不是一种 mode，而是每种 mode 固定的支付规则（Table A），所以不需要 `NeedImplementation`。
- “contract evidence”和“usage evidence”是同一个问题的两个名字（从使用方反推约束），合并为 `Usage`。
- 方向（In / Out）是 **traversal** 的属性（从哪个起始 state 出发），不是 state 的属性。Out traversal 内部也可能出现 `Usage` state（见 D2），这不影响它属于 `R_out`。

### 7.1 State 去重与 Cost 去重必须分开

Solver 可以多次访问同一个 node 的不同 reasoning state：

```text
visited_states: Set<(NodeId, Mode)>
```

但同一个 source fragment 的 token cost 只能计算一次：

```text
reached_fragments: Set<ContextFragmentId>
```

因此：

> **reasoning state 可以重复到达同一 artifact；source context 不重复计费。**

注意：`reached_fragments` 是 **每次 traversal 独立的**。Out 和 In 两次 traversal 各自维护自己的集合，得到 `R_out` 和 `R_in`；总 CF 在事后取 union。如果两次 traversal 共享一个“已付费”集合并据此跳过，`R_in` 会漏掉被 `R_out` 先付的 fragment，`CF_in` 系统性偏低。

### 7.2 Transition Rules

算法的全部语义由两张表决定。伪代码（§10）里的 `pay(state)`、`derive_relations(state)`、`next_state(...)` 都只是查表。

**Table A — 处理一个 state 时支付什么**

| State | 支付 |
|---|---|
| `(f, Behavior)` | `pay_surface(f)` + `implementation(f)` |
| `(S, Provenance)` | `pay_surface(S)` |
| `(f, Usage)` | `pay_surface(f)` + `implementation(f)` |

一个 state 被入队，就意味着已经判定需要它的 implementation（Out 方向：callee 的 contract 不足；In 方向：call site 在 body 里）。因此 Table A 不需要条件。变量没有独立的 implementation fragment（initializer 属于 surface）。

**Table B — 从一个 state 派生哪些 reasoning relation**

列说明：

- **遇到时支付**：发现这条 relation 时无条件支付的内容，先于决策。
- **决策**：`policy(x)` 表示调用 `BoundaryPolicy.evaluate`，subject 为 `x`；`fact` 表示由 program fact 直接决定；`—` 表示总是停止。
- **Transparent 时 push**：决策为 `Transparent`（或 `Unknown`）时入队的下一个 state。
- target 为 external 时：支付“遇到时支付”列中的 surface 部分后直接停止，不调用 policy（§8）。

*Out 方向 — `(f, Behavior)`，沿 `f` 的出边：*

| Program fact | Relation | 遇到时支付 | 决策 | Transparent 时 push |
|---|---|---|---|---|
| `f --Call--> g` | needs behavior of `g` | `pay_surface(g)` | `policy(g)` | `(g, Behavior)` |
| `f --Read--> S`, `S` Const/Immutable | needs value of `S` | `pay_surface(S)` | `fact` → Boundary | — |
| `f --Read--> S`, `S` Mutable | needs value of `S` | `pay_surface(S)` | `fact` → Transparent | `(S, Provenance)` |
| `f --Write--> S` | needs declaration of `S` | `pay_surface(S)` | — | — |
| `f --Annotates--> d` | needs behavior of `d` | `pay_surface(d)` | `policy(d)` | `(d, Behavior)` |
| `f --OverriddenBy--> f_i` | needs behavior of implementor | `pay_surface(f_i)` | `policy(f_i)` | `(f_i, Behavior)` |

说明：`OverriddenBy` 行只在 `f` 本身已处于 `Behavior` state 时才会被派生——也就是说 `f`（abstract / interface method）的 contract 已经被判为不足，所以需要看实现。每个实现再独立过一次 policy：子类可能有比 parent 更完整的 contract。

*Out 方向 — `(S, Provenance)`，沿 `S` 的入边：*

| Program fact | Relation | 遇到时支付 | 决策 | Transparent 时 push |
|---|---|---|---|---|
| `w --Write--> S` | value provenance from `w` | `pay_surface(w)` | `policy(w)`，默认 Transparent | `(w, Behavior)` 且 `(w, Usage)` |

说明：理解 `w` 写了什么，需要 `w` 的行为（写入值可能来自 callee）也需要 `w` 的输入（写入值可能来自参数，即来自 caller）。后者就是 `(w, Usage)`，它会被 `w` 自身的 contract gate（见下表）。这保持了旧算法“writer 也触发 call-in exploration”的语义。见 D2。

*In 方向 — `(f, Usage)`：*

这一组 relation 由一个 **gate** 统一控制：`policy(f)`，即 `f` **自身** 的 contract 是否足够。如果为 `Boundary`，下表中标注 *gated* 的行一条都不派生；否则全部派生。Gate 对每个 `f` 只评估一次。

| Program fact | Relation | Gate | 遇到时支付 | 决策 | Transparent 时 push |
|---|---|---|---|---|---|
| `p --OverriddenBy--> f`（`f` 覆盖 `p`） | obligation from parent contract | 无 | `pay_surface(p)` | — | — |
| `p --OverriddenBy--> f` | callers via parent | gated | — | `fact` → Transparent | `(p, Usage)` |
| `c --Call--> f` | usage evidence from caller | gated | `pay_surface(c)` + `implementation(c)` | `fact` → Transparent | `(c, Usage)` |
| `f --OverriddenBy--> f_i`（`f` 被覆盖） | obligation evidence from override | gated | `pay_surface(f_i)` + `implementation(f_i)` | — | — |
| `f --Write--> S`, `S` Mutable, `r --Read--> S` | observed by reader | gated | `pay_surface(S)` + `pay_surface(r)` + `implementation(r)` | — | — |

说明：

- caller 行的“决策”是 `fact` 而不是 `policy(c)`：是否继续到 `c` 的 callers，在处理 `(c, Usage)` 时由 `c` 自己的 gate 决定。这样 policy 对每个函数只回答一次同一个问题。
- `(c, Usage)` **不**派生 `c` 的出边（不展开 `c` 的 callee）。
- 最后两行是一跳终止：overrider 和 reader 作为 evidence 被计费，但不再从它们向外派生（D4、D5）。
- Usage mode 下没有 `Annotates` 行：decorator 影响的是 `f` 的行为（Out），不是外部对 `f` 的约束。

**起始 state**

```text
R_out(u) = traverse from (u, Behavior)
R_in(u)  = traverse from (u, Usage)
```

两侧都会支付 `pay_surface(u) + implementation(u)`（Table A），union 后只计一次（9.1）。

### 7.3 Design Decisions and Ablation Candidates

下列决定不是由 metric 定义唯一确定的，这里给出默认选择、理由和替代方案。替代方案是实验中 ablation 的候选。

| ID | 决定 | 默认 | 理由 | 替代方案 |
|---|---|---|---|---|
| D1 | 签名中的类型是否计费 | 支付 type surface（6.1.1） | 否则 boundary 可以把成本藏进类型 | 不计；或只计类型名 |
| D2 | Provenance 到达 writer 后如何展开 | `(w, Behavior)` + `(w, Usage)` | 写入值既可能来自 callee 也可能来自参数；与旧算法语义一致 | 只 `(w, Behavior)`；或仅支付 `implementation(w)` 不再展开 |
| D3 | CF-in 是否多跳 | 多跳，由每个 caller 自身的 contract gate | 透传未标注参数的 caller 不提供新 evidence | 固定一跳 |
| D4 | overrider 作为 obligation evidence 时是否继续 | 一跳终止 | overrider 的 caller 属于另一个方法的 usage | 推 `(f_i, Usage)` |
| D5 | 写入共享状态时是否把 reader 计入 CF-in | 计入，一跳终止 | 与 3.3 对称；修改写入语义需知道谁在观察 | 不计；或推 `(r, Behavior)` |
| D6 | `Unknown` 的默认 | Transparent | conservative principle | Boundary（作为下界估计） |

其中 D2、D3、D5 是 CF 数值爆炸的主要来源，也是“shared mutable state 惩罚”和“under-specified API 惩罚”的来源。实验中应报告它们对结果的敏感性。

---

## 8. External Dependencies

External library 不再因为“第三方 API 默认可靠”而被视为 boundary。

更明确的定义是：

> **CF 默认测量 repository-local structural context depth。External implementation 位于 measurement universe 之外。**

因此 external dependency 的规则是：

```text
pay external API surface
stop
```

`surface` 可以包括：

- imported symbol signature；
- parameter / return type definitions；
- externally visible docs available to the analyzer。

但不会进入 third-party implementation source。

这是 **measurement boundary**，不是对第三方设计质量的评价。

如果未来需要分析 monorepo / vendored dependency / workspace dependency，可以通过 measurement scope 配置决定某个 package 是否仍然属于 internal universe。

---

## 9. CF Computation

给定起始 function `u`：

```text
R_out(u) = context fragments reached by Out reasoning traversal
R_in(u)  = context fragments reached by In reasoning traversal
```

定义：

\[
CF_{out}(u)=Size(R_{out}(u))
\]

\[
CF_{in}(u)=Size(R_{in}(u))
\]

总 footprint：

\[
CF(u)=Size(R_{out}(u) \cup R_{in}(u))
\]

如果需要展示 additive decomposition，可以额外报告 overlap：

\[
CF(u)=CF_{out}(u)+CF_{in}(u)-CF_{overlap}(u)
\]

其中：

\[
CF_{overlap}(u)=Size(R_{out}(u) \cap R_{in}(u))
\]

### 9.1 Target Unit

起始 unit 自身的 `pay_surface(u) + implementation(u)` 属于两侧 reasoning 的共同基点：它同时属于 `R_out(u)` 和 `R_in(u)`，在总 CF 的 union 中只计一次。因此 `CF_out(u)` 和 `CF_in(u)` 各自都包含 `u` 本身，而 `CF_overlap(u) >= size(u)`。

`R_out` 和 `R_in` 由两次 **独立** traversal 产生，不共享已付费集合（见 7.1）。

### 9.2 Cycle Handling

Program graph 与 reasoning graph 都可能包含环：

- mutual recursion；
- shared-state feedback；
- decorator loops；
- inheritance relationships；
- callback cycles。

通过 `visited_states` 防止同一 reasoning state 无限循环，通过 `reached_fragments` 防止 context 重复计费。

### 9.3 Truncation

`max_tokens`、`max_nodes`、timeout 等参数仅用于控制 analyzer 的执行资源。

它们 **不属于 CF 定义**。

Limits 对两次 traversal **各自** 生效（否则 Out 耗尽预算会让 In 看起来为零）。如果任一方向被截断，该方向的值和总 CF 都只是下界：

```text
CfResult {
    cf_out, cf_in, cf_total,     // 截断时为 lower bound
    truncated_out: bool,
    truncated_in: bool,
    pending_states,              // 截断时队列中尚未处理的 state
    unresolved,                  // policy 返回 Unknown 的 relation
}
```

例如：

```text
CF >= 128k tokens (truncated)
```

而不能把 `128k` 当作真实 CF。

---

## 10. Solver Pseudocode

```text
function compute_cf(graph, u, policy, size_fn, limits):
    out = traverse(graph, (u, Behavior), policy, limits)
    in_ = traverse(graph, (u, Usage),    policy, limits)

    return CfResult {
        cf_out:        size_fn(out.fragments),
        cf_in:         size_fn(in_.fragments),
        cf_total:      size_fn(out.fragments UNION in_.fragments),
        cf_overlap:    size_fn(out.fragments INTERSECT in_.fragments),
        truncated_out: out.truncated,
        truncated_in:  in_.truncated,
        pending_states: out.pending UNION in_.pending,
        unresolved:    out.unresolved UNION in_.unresolved,
        boundary_stops: out.stops UNION in_.stops,
    }
```

核心 traversal。注意它不包含任何 relation-specific 逻辑：`pay`、`derive_relations`、`next_state` 都是对 7.2 两张表的查表。

```text
function traverse(graph, initial_state, policy, limits):
    queue    = Queue([initial_state])
    visited  = set()          // Set<(NodeId, Mode)>
    reached  = set()          // Set<ContextFragmentId>
    stops    = []             // relations 在哪里被 Boundary 截止（可解释性）
    unresolved = []           // policy 返回 Unknown 的 relations

    while queue not empty:
        if limits exceeded:
            return { fragments: reached, truncated: true, pending: queue, stops, unresolved }

        state = queue.pop_front()
        if state in visited:
            continue
        visited.add(state)

        // Table A
        pay(state, reached)

        // Table B（含 Usage gate）
        for rel in derive_relations(graph, policy, state):
            pay_on_encounter(rel, reached)          // Table B “遇到时支付”列

            if rel.target.is_external:
                continue                            // §8: measurement boundary

            decision = rel.decision                 // Table B “决策”列
            if decision is PolicyCall:
                decision = policy.evaluate(state.mode, rel.kind, rel.subject, rel.other, graph.type_registry)
                if decision == Unknown:
                    unresolved.append(rel)
                    decision = Transparent          // D6

            if decision == Boundary:
                stops.append(rel)                   // 一个 boundary 成功阻止了展开
                continue
            if rel.next is None:
                continue                            // 一跳终止行（Write / overrider / reader evidence）

            for next in rel.next:                   // Table B “Transparent 时 push”列，可能多个（D2）
                queue.push_back(next)

    return { fragments: reached, truncated: false, pending: [], stops, unresolved }
```

两个查表函数的形状：

```text
function pay(state, reached):
    node = graph.node(state.node_id)
    pay_surface(node, reached)                      // surface + 签名引用的 type surfaces（6.1.1）
    if state.mode in {Behavior, Usage}:
        reached.add(node.implementation_fragment)

function derive_relations(graph, policy, state):
    match state.mode:
        Behavior   => rows of Table B / Behavior   for outgoing edges of node
        Provenance => rows of Table B / Provenance for incoming Write edges of node
        Usage      =>
            rels = [obligation-from-parent rows]    // 不受 gate 控制
            if policy.evaluate(Usage, OwnContract, node, None, registry) != Boundary:
                rels += [gated rows]                // callers, callers via parent, overriders, readers
            return rels
```

与旧算法（`design.md` 1.4）的行为差异，实现时需要注意：

1. **两次独立 traversal** 代替一次混合 traversal，得到可分解的 `CF_out` / `CF_in`。
2. **caller 不再展开其 callee**。旧算法把 caller 入队后会对它做完整 forward traversal，这是一个 over-inclusion。
3. **surface / implementation 拆分**。Boundary 只付 surface，旧算法付整个 `context_size`。
4. **type surface 计费**（D1），旧算法没有。
5. **写共享状态的 reader 计入 CF-in**（D5），旧算法没有。
6. `Write` 边在 Out 方向只付 `surface(S)`。旧算法标为 Transparent 并支付整个变量节点，但不触发 writer exploration，效果上相同。
7. `visited` 的 key 从 `NodeId` 变为 `(NodeId, Mode)`。

保持一致的部分：external 总是停止；Const/Immutable 变量总是 Boundary；mutable 变量经 Read 到达时展开 writers；writer 同时触发 call-in（D2）；contract 完整的函数不展开 callers；“通过 Call 到达的函数不探索 callers”在新模型中自动成立（`Behavior` state 不派生 caller 行）。

---

## 11. Graph Schema（Part 1 ↔ Part 2）

### 11.1 Node Model

```text
enum Node {
    Function(FunctionNode),
    Variable(VariableNode),
}

struct NodeCore {
    id: NodeId,
    name: String,
    scope: Option<ScopeId>,

    surface_fragment: ContextFragment,
    implementation_fragment: Option<ContextFragment>,

    span: SourceSpan,
    documentation: Option<String>,
    is_external: bool,
    file_path: String,
}
```

`ContextFragment`：

```text
struct ContextFragment {
    id: ContextFragmentId,
    span: Option<SourceSpan>,
    normalized_text: Option<String>,
    context_size: u32,
}
```

Function：

```text
struct FunctionNode {
    core: NodeCore,
    parameters: Vec<Parameter>,
    return_types: Vec<TypeId>,
    is_async: bool,
    is_generator: bool,
    visibility: Visibility,
    owner_type: Option<TypeId>,
    is_interface_method: bool,
}
```

Variable：

```text
struct VariableNode {
    core: NodeCore,
    var_type: Option<TypeId>,
    mutability: Mutability,       // Const | Immutable | Mutable
    variable_kind: VariableKind, // Global | ClassField | Local
    type_source: TypeSource,
}
```

### 11.2 Edge Model

```text
enum EdgeKind {
    Call,
    Read,
    Write,
    OverriddenBy,
    Annotates,
}
```

Solver 内部的 traversal 类型（不属于 Graph Schema，但列在这里便于对照 §7）：

```text
enum ReasoningMode { Behavior, Provenance, Usage }

struct TraversalState { node_id: NodeId, mode: ReasoningMode }

enum RelationKind {
    // Behavior
    NeedsBehavior,          // Call / Annotates / OverriddenBy(implementor)
    NeedsValue,             // Read
    NeedsDeclaration,       // Write
    // Provenance
    ValueProvenance,        // incoming Write
    // Usage
    OwnContract,            // gate
    ParentObligation,       // p OverriddenBy f
    CallersViaParent,       // p OverriddenBy f, gated
    UsageEvidence,          // incoming Call
    OverrideEvidence,       // f OverriddenBy f_i
    ObservedByReader,       // f Write S, r Read S
}
```

仍然不需要 `CallIn` 或 `SharedStateWrite` 边。

它们属于 Reasoning Dependency 推导，而不是 Program Fact Graph 本身。

### 11.3 Type Registry

```text
struct TypeRegistry {
    types: HashMap<TypeId, TypeInfo>,
    implementors: HashMap<TypeId, Vec<TypeId>>,
}

struct TypeInfo {
    definition: TypeDefAttribute,
    surface_fragment: ContextFragment,
    documentation: Option<String>,
}

struct TypeDefAttribute {
    type_kind: TypeKind,
    is_abstract: bool,
    type_param_count: u32,
    type_var_info: Option<TypeVarInfo>,
}
```

Type Registry 是 BoundaryPolicy 和 relation derivation 的输入，而不是 CF metric 本身的一部分。

---

# Part 2: Graph Building

## 12. Builder Responsibility

Graph Builder 的职责只有：

> **把语言无关 Semantic Data 转换为完整、保守的 Program Fact Graph。**

Builder 不判断某个 boundary 是否“好”，也不决定 traversal 是否停止。

这使得 Graph Builder 可以单独测试：

- symbol resolution 是否正确；
- Call / Read / Write 是否完整；
- override / implementation 关系是否完整；
- type propagation 是否正确；
- external symbol 是否正确建模。

### 12.1 Conservative Graph Principle

Graph 必须尽量避免 false negative：

> 如果某个 program relationship 可能影响 reasoning，Builder 应把它保留下来。

false positive 会让 CF 更保守；false negative 则可能让 CF 错误偏低。

不同实现可以采用不同精度：

- AST lexical resolution；
- class hierarchy analysis；
- LSP type information；
- points-to analysis；
- compiler semantic model。

Graph precision 是 CF 的显式 implementation variable，但不改变 metric definition。

---

## 13. Semantic Data Schema（Part 2 ↔ Part 3）

保留语言无关语义结构：

```text
struct SemanticData {
    documents: Vec<Document>,
    external_symbols: Vec<ExternalSymbol>,
}

struct Document {
    path: String,
    definitions: Vec<Definition>,
    references: Vec<Reference>,
}
```

Definition：

```text
struct Definition {
    symbol_id: SymbolId,
    name: String,
    kind: DefinitionKind,        // Function | Variable | Type
    span: SourceSpan,
    documentation: Option<String>,
    enclosing_symbol: Option<SymbolId>,
    is_external: bool,
    details: DefinitionDetails,
}
```

Reference：

```text
struct Reference {
    enclosing_symbol: SymbolId,
    target_symbol: Option<SymbolId>,
    role: ReferenceRole,         // Call | Read | Write | Decorate
    receiver: Option<SymbolId>,
    method_name: Option<String>,
    assigned_to: Option<SymbolId>,
}
```

---

## 14. Builder Passes

### Pass 1: Node Allocation + TypeRegistry

- Type → 注册到 TypeRegistry；
- Function / Variable → 创建 graph node；
- 生成 surface / implementation fragments；
- 构造 symbol / enclosing / owner indexes；
- 构造 type implementor index。

### Pass 2: Static Forward Edges

从 references 构造：

- Call；
- Read；
- Write；
- Annotates。

未解析调用进入 `unresolved_calls`。

### Pass 2.5: Type Propagation

填充：

- parameter types；
- return types；
- variable types；
- external call return type propagation；
- receiver type information。

### Pass 3: OverriddenBy + Edge Recovery

- 根据 TypeRegistry 构造 `OverriddenBy`；
- 根据 receiver type 等信息恢复 unresolved calls；
- 迭代至不动点或无更多可恢复关系。

注意：

> Builder 不构造 reasoning-specific reverse edges。

`CallIn`、shared-state writer exploration 等在 Part 1 根据 program facts 动态推导。

---

# Part 3: Language-specific Extractor

## 15. Extractor Responsibility

Extractor 只负责把语言语义映射到 `SemanticData`。

对于 Python，需要提取：

1. Definitions：function / method / variable / type；
2. References：Call / Read / Write / Decorate；
3. Type information；
4. Symbol hierarchy / enclosing relationships；
5. inheritance / protocol / ABC relationships；
6. external symbol metadata。

可以基于：

- Python AST；
- Pyright / Pylance；
- tree-sitter + type engine；
- 其他 LSP / compiler frontend。

只要输出满足 `SemanticData` contract，后续 Builder 与 CF Query 无需知道语言实现细节。

---

# 16. Explicit Extension Points

CF 的核心定义固定，但实现允许在几个位置替换策略。

## 16.1 Size Function

```text
size(fragment)
```

默认：LLM token count。

可替换：AST nodes、lexical tokens、normalized tokens 等。

## 16.2 Boundary Policy

```text
BoundaryPolicy.evaluate(...)
```

决定 surface 是否足以停止 traversal。

简单 syntactic heuristic、静态 contract analysis、LLM evaluator 都可以作为不同实现。

## 16.3 Graph Construction Precision

影响 program facts 的 false positive / false negative。

## 16.4 Measurement Scope

定义哪些 package / repository / workspace 属于 CF 的 local measurement universe。

external boundary 是 scope rule，不是 boundary-quality judgment。

---

# 17. Recommended Output

CF Analyzer 不应只返回一个数字。

建议返回：

```text
CfResult {
    target: SymbolId,

    cf_total: u64,
    cf_out: u64,
    cf_in: u64,
    cf_overlap: u64,

    out_fragments: Vec<ContextFragmentId>,
    in_fragments: Vec<ContextFragmentId>,

    boundary_stops: Vec<BoundaryStop>,      // (state, relation, subject) 在哪里被 Boundary 截止
    unresolved: Vec<UnresolvedRelation>,    // policy 返回 Unknown 的 relation
    pending_states: Vec<TraversalState>,    // 截断时未处理的 state

    truncated_out: bool,
    truncated_in: bool,
    graph_coverage: CoverageInfo,
    boundary_policy_id: String,
    size_function_id: String,
}
```

这样 CF 才具有可解释性：

- 为什么这个函数 CF 高？
- 哪些 boundary 成功阻止了 traversal？
- 哪些 boundary 被判定为 transparent？
- CF_in 是被大量 caller 拉高，还是 CF_out 被 hidden state 拉高？
- 重构前后的 CF 变化来自哪里？

这些信息对于把 CF 用作 design feedback 比单一 scalar 更有价值。

---

# 18. Interpretation

CF 的解释应保持克制。

### 可以说

- `A` 比 `B` 需要加载更多 structural context 才能理解；
- 同一 responsibility 重构后 CF 显著降低；
- 某个 nominal abstraction 没有形成有效 cut point；
- shared mutable state 造成了显著 CF-out expansion；
- under-specified API 导致 CF-in 扩张到大量 usage sites。

### 不应该直接说

- 高 CF 的函数一定设计差；
- 低 CF 的函数一定设计好；
- 两个承担不同问题的函数可以用 CF 排名“质量”；
- CF 已经测量了完整的 Context Cost；
- CF 可以替代 Breadth、complexity、correctness 等指标。

CF 的核心价值是：

> **把“这个 boundary 到底有没有真正减少需要理解的上下文”变成一个可计算、可比较的问题。**

对于软件设计研究，最可靠的使用方式仍然是比较 equivalent designs，尤其是重构前后：

> **same responsibility, same behavior, less required structural context.**

这就是 CF 希望测量的变化。
