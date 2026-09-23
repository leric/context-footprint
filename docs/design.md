> [!NOTE]
> This document describes the pre-CF-in/CF-out implementation and is retained as
> migration history. [`alg.md`](alg.md) is the normative algorithm specification;
> [`algorithm-migration-analysis.md`](algorithm-migration-analysis.md) records the
> differences and migration rationale.

## Architecture Overview

CF 的实现分为三个独立模块，通过两个数据协议（Schema）隔离复杂度：

```
Semantic Data Schema              Graph Schema
       ↓                              ↓
[Part 3: Extractor]  →  [Part 2: Graph Builder]  →  [Part 1: CF Query]
 (language-specific)      (language-agnostic)         (graph algorithm)
```

- **Graph Schema**（`ContextGraph` + `TypeRegistry`）：Part 1 和 Part 2 之间的数据协议。定义了图的节点、边、类型注册表的完整结构。
- **Semantic Data Schema**（`SemanticData` JSON）：Part 2 和 Part 3 之间的数据协议。定义了语言无关的语义提取结果结构。

每个模块只依赖其输入侧的 Schema，不需要了解其他模块的内部逻辑。

> 补充说明：`SemanticData` 的 **JSON shape** 在本文定义；Extractor 的 **行为正确性要求**
> 由 [extractor-spec.md](./extractor-spec.md) 定义。后者包括抽象引用模式、跨语言正确性不变量、
> 以及 conformance tests 要求。

---

## Part 1: CF Query

基于 Graph Schema 定义的图结构，实现 CF 的遍历与计算。

### 1.1 Graph Definition

将代码库建模为有向图 $G = (V, E)$：

**Nodes** $V$：代码单元，仅包含 **函数** 和 **变量**。类型定义不是图节点，存储在独立的 **Type Registry** 中，由节点通过 type ID 引用。

**Edges** $E$：所有边均为 **正向依赖**（forward dependency），从使用者指向被使用者。五种边类型：

| **Edge Kind** | **Direction** | **Semantics** |
| --- | --- | --- |
| `Call` | Function → Function | 函数调用 |
| `Read` | Function → Variable | 读取变量值 |
| `Write` | Function → Variable | 修改变量值 |
| `OverriddenBy` | Parent Method → Child Method | 方法覆盖（统一处理 interface implementation 和 concrete override） |
| `Annotates` | Decorated → Decorator | 装饰器关系 |

> **关键变更**：不再有 `SharedStateWrite` 和 `CallIn` 边类型。反向探索（shared-state write exploration 和 call-in exploration）在遍历时通过访问节点的 **incoming edges** 实现，不需要预先物化为图中的边。
> 

**Type Registry**：存储类型定义（Class, Interface, Struct, Enum, TypeAlias, TypeVar），提供类型属性查询（`is_abstract`, `type_param_count`, `type_var_info`）和 `implementors` 反向索引（parent type → child types，包含 interface implementation 和 concrete inheritance）。

**Node Attributes**：

- $size(v)$：节点的上下文大小（token count）
- $doc\_score(v)$：文档质量评分 $\in [0.0, 1.0]$
- $is\_external(v)$：是否为第三方库节点
- Function 节点额外属性：`parameters`（含 `param_type`）, `return_types`, `is_interface_method`（仅影响 `context_size` 计算：interface method 仅用 signature span）
- Variable 节点额外属性：`var_type`, `mutability`（Const/Immutable/Mutable）, `variable_kind`

### 1.2 Traversal Algorithm

CF 通过 BFS 遍历计算从起点 $u_0$ 可达的节点集 $R(u_0)$。遍历包含两个方向：

**Forward traversal**：沿出边（outgoing edges）遍历，覆盖五种正向依赖。

**Reverse exploration**：在特定条件下，沿入边（incoming edges）反向探索。两种场景：

1. **Call-in exploration**：当到达一个规范不完整的函数 $v$，且 $v$ 不是通过 Call 边到达时，沿 $v$ 的 incoming Call edges 反向遍历到所有调用者——理解 $v$ 实际接收什么参数。
2. **Shared-state write exploration**：当到达一个可变变量 $S$（通过 Read 边），沿 $S$ 的 incoming Write edges 反向遍历到所有写入者——理解 $S$ 的可能取值。

> 这两种 reverse exploration 不引入新的边类型，而是在遍历时访问现有边的反方向。
> 

**CF 计算**：

$$
CF(u_0) = \sum_{v \in R(u_0)} size(v)
$$

### 1.3 Edge-Aware Pruning Predicate $P(E_{in}, v, E_{out})$

对于当前节点 $v$ 的每条候选探索路径（无论是正向出边还是反向入边），根据到达 $v$ 的入边 $E_{in}$ 独立判断是否继续。返回 **Boundary**（停止）或 **Transparent**（继续）。

**Core Asymmetry**：正向边检查 *目标* 的规范完整性；反向探索检查 *来源* 的规范完整性。但 incoming edge 的上下文可以 override 这些默认规则。

#### Forward Edge Rules

**Call edge** → 检查 target 函数 $v$：

- $v$ is external → **Boundary**
- $v$ is interface method with complete signature + doc → **Boundary**
- $v$ is abstract factory（return type is abstract with doc）→ **Boundary**
- Academic mode: $v$ has complete signature + doc → **Boundary**
- Otherwise → **Transparent**

**Read edge** → 检查 target 变量 $v$ 的可变性：

- $v$ is Const or Immutable → **Boundary**
- $v$ is Mutable → **Transparent**（触发 shared-state write exploration）

**Write edge** → 始终 **Transparent**

**OverriddenBy edge** → 使用标准函数剪枝规则评估 target（仅在父类方法为 Transparent 时可达）

**Annotates edge** → 使用标准函数剪枝规则评估 target

#### Reverse Exploration Rules

**Call-in exploration**（从函数 $v$ 沿 incoming Call edges 到调用者）：

- $v$ was reached via Call edge → **不探索**（调用上下文已知）
- $v$ has complete specification + doc → **不探索**（规范足够）
- Otherwise → **探索所有调用者**

**Shared-state write exploration**（从可变变量 $S$ 沿 incoming Write edges 到写入者）：

- **始终探索**——理解可变共享状态的唯一方式是知道所有写入者

> **Conservative Principle**: When in doubt, traverse. 确保 CF 始终是推理上下文的保守上界。
> 

### 1.4 Algorithm Pseudocode

```
function compute_cf(graph, start_node, pruning_params, max_tokens):
    visited = empty_set()
    queue = empty_queue()
    total_size = 0

    queue.enqueue((start_node, depth=0, incoming_edge=None))

    while queue is not empty:
        (current, depth, incoming_edge) = queue.dequeue()
        current_node = graph.node(current)

        if current_node.id in visited:
            continue

        visited.add(current_node.id)
        total_size += current_node.context_size

        if max_tokens is not None AND total_size >= max_tokens:
            break

        // === Forward traversal: outgoing edges ===
        for (neighbor, edge_kind) in graph.outgoing_edges(current):
            neighbor_node = graph.node(neighbor)
            decision = evaluate_forward(pruning_params, current_node, 
                                        neighbor_node, edge_kind, graph)
            if decision == Transparent:
                queue.enqueue((neighbor, depth + 1, edge_kind))
            else:  // Boundary — include node but don't traverse further
                if neighbor_node.id not in visited:
                    visited.add(neighbor_node.id)
                    total_size += neighbor_node.context_size

        // === Reverse exploration: incoming edges ===
        // 1. Call-in exploration (for functions)
        if current_node is Function:
            if should_explore_callers(current_node, incoming_edge, pruning_params):
                for (caller, edge_kind) in graph.incoming_edges(current, filter=Call):
                    queue.enqueue((caller, depth + 1, CallIn_marker))

        // 2. Shared-state write exploration (for mutable variables reached via Read)
        if current_node is Variable AND current_node.mutability == Mutable:
            if incoming_edge == Read:
                for (writer, edge_kind) in graph.incoming_edges(current, filter=Write):
                    queue.enqueue((writer, depth + 1, SharedStateWrite_marker))

    return CfResult { reachable_set: visited, total_context_size: total_size }

function should_explore_callers(func_node, incoming_edge, params):
    // Already arrived via Call — caller context is known
    if incoming_edge == Call:
        return false
    // Specification complete — no need to check usage
    if func_node.is_signature_complete() AND
       func_node.doc_score >= params.doc_threshold:
        return false
    return true

function evaluate_forward(params, source, target, edge_kind, graph):
    // External always stops
    if target.is_external:
        return Boundary

    // Variable targets
    if target is Variable:
        if edge_kind == Write:
            return Transparent
        // Read edge
        if target.mutability in [Const, Immutable]:
            return Boundary
        return Transparent

    // Function targets
    if target is Function:
        if target.is_interface_method:
            if target.is_signature_complete() AND
               target.doc_score >= params.doc_threshold:
                return Boundary
            return Transparent

        if is_abstract_factory(target, graph.type_registry, params.doc_threshold):
            return Boundary

        if params.academic_mode AND
           target.is_signature_complete() AND
           target.doc_score >= params.doc_threshold:
            return Boundary

        return Transparent
```

### 1.5 Graph Schema（Data Contract: Part 1 ↔ Part 2）

Graph Schema 定义了 CF Query 所需的完整图结构。Graph Builder 的唯一职责是按此 Schema 构建图。

**ContextGraph Structure**：

- Directed graph: `petgraph::DiGraph<Node, EdgeKind>`（支持 outgoing 和 incoming edge 查询）
- Symbol lookup: `HashMap<SymbolId, NodeIndex>`
- TypeRegistry: 类型定义（不在图中）

```
enum EdgeKind {
    Call,            // Function → Function
    Read,            // Function → Variable
    Write,           // Function → Variable
    OverriddenBy,    // Parent Method → Child Method (implement + override)
    Annotates,       // Decorated → Decorator
}
```

> **注意**：不再有 `SharedStateWrite` 和 `CallIn` 边类型。反向探索在遍历时通过 `graph.incoming_edges()` 实现。
> 

**Node Types**：

```
enum Node {
    Function(FunctionNode),
    Variable(VariableNode),
}

struct NodeCore {
    id: NodeId,
    name: String,
    scope: Option<ScopeId>,
    context_size: u32,
    span: SourceSpan,
    doc_score: f32,
    is_external: bool,
    file_path: String,
}

struct FunctionNode {
    core: NodeCore,
    parameters: Vec<Parameter>,     // name + param_type: Option<TypeId>
    return_types: Vec<TypeId>,
    is_async: bool,
    is_generator: bool,
    visibility: Visibility,
    is_interface_method: bool,
}

struct VariableNode {
    core: NodeCore,
    var_type: Option<TypeId>,
    mutability: Mutability,         // Const | Immutable | Mutable
    variable_kind: VariableKind,    // Global | ClassField
    type_source: TypeSource,        // Annotation | Inferred | ExternalCallReturn | Unknown
}
```

**TypeRegistry**：

```
struct TypeRegistry {
    types: HashMap<TypeId, TypeInfo>,
    implementors: HashMap<TypeId, Vec<TypeId>>,  // parent type → child types (implement + inherit)
}

struct TypeInfo {
    definition: TypeDefAttribute,
    context_size: u32,
    doc_score: f32,
}

struct TypeDefAttribute {
    type_kind: TypeKind,    // Class, Interface, Struct, Enum, TypeAlias, TypeVar
    is_abstract: bool,
    type_param_count: u32,
    type_var_info: Option<TypeVarInfo>,
}

struct TypeVarInfo {
    bound: Option<TypeId>,
    constraints: Vec<TypeId>,
}
```

**Signature Completeness**（用于 pruning 判断）：

- `is_signature_complete_with_registry(type_registry)` = 所有参数 effectively typed + 有 return type
- Unbounded TypeVar 参数不算 effectively typed（≈ `Any`）

**Pruning Modes**：

```
struct PruningParams {
    doc_threshold: f32,
    academic_mode: bool,   // true: typed+documented function → Boundary
}
// Academic: PruningParams { doc_threshold: 0.5, academic_mode: true }
// Strict:   PruningParams { doc_threshold: 0.8, academic_mode: false }
```

---

## Part 2: Graph Building

基于 Semantic Data Schema，构建符合 Graph Schema 的 `ContextGraph`。

### 2.1 Semantic Data Schema（Data Contract: Part 2 ↔ Part 3）

Semantic Data 是语言无关的语义提取结果，由 Part 3 的语言特定 Extractor 生成，Part 2 的 Builder 消费。

**核心结构**：

```
struct SemanticData {
    documents: Vec<Document>,
    external_symbols: Vec<ExternalSymbol>,  // 第三方库符号
}

struct Document {
    path: String,                    // 文件路径
    definitions: Vec<Definition>,    // 该文件中的所有定义
    references: Vec<Reference>,      // 该文件中的所有引用
}

struct Definition {
    symbol_id: SymbolId,             // 全局唯一标识
    name: String,
    kind: DefinitionKind,            // Function | Variable | Type
    span: SourceSpan,                // 源码位置
    documentation: Option<String>,
    enclosing_symbol: Option<SymbolId>,  // 所属的父符号
    is_external: bool,
    details: DefinitionDetails,      // kind-specific 详情
}

enum DefinitionKind { Function, Variable, Type }

struct DefinitionDetails {
    // Function details
    parameters: Option<Vec<ParamDef>>,
    return_types: Option<Vec<TypeId>>,
    modifiers: Option<Modifiers>,         // is_abstract, is_async, etc.

    // Variable details
    var_type: Option<TypeId>,
    mutability: Option<Mutability>,
    variable_kind: Option<VariableKind>,

    // Type details
    type_kind: Option<TypeKind>,
    is_abstract: Option<bool>,
    inherits: Option<Vec<TypeId>>,
    implements: Option<Vec<TypeId>>,
    type_params: Option<Vec<TypeParamDef>>,
}

struct Reference {
    enclosing_symbol: SymbolId,       // 引用所在的符号
    target_symbol: Option<SymbolId>,  // 引用的目标（None = unresolved）
    role: ReferenceRole,              // Call | Read | Write | Decorate
    receiver: Option<SymbolId>,       // method call 的 receiver 变量
    method_name: Option<String>,      // method call 的方法名
    assigned_to: Option<SymbolId>,    // 调用结果赋值给哪个变量
}

enum ReferenceRole { Call, Read, Write, Decorate }
```

> Extractor 负责从语言特定的 AST/LSP 数据中提取上述结构。不同语言实现不同的 Extractor，但输出统一的 `SemanticData` JSON。
> 但“结构合法”不等于“语义正确”。Extractor 还必须满足 [extractor-spec.md](./extractor-spec.md)
> 中定义的正确性不变量：例如 symbol identity 稳定、属性查询命中 referent token、链式访问基于
> 中间表达式的值类型、以及在信息不足时宁可 unresolved 也不生成伪 symbol。
> 

### 2.2 Builder Algorithm

Builder 将 `SemanticData` 转换为 `ContextGraph`。由于正向引用和类型传播的存在，使用 **多 Pass 策略**：

#### Pass 1: Node Allocation + TypeRegistry

遍历所有 `Definition`：

- **Type** → 注册到 `TypeRegistry`（不创建图节点）；注册 `inherits`/`implements` 到 `implementors` 索引
- **Function / Variable** → 创建图节点，计算 `context_size`（interface method 仅用 signature span）和 `doc_score`
- 构建 `init_map`：type_symbol → `__init__` 方法节点（用于构造函数调用解析）

#### Pass 2: Edge Wiring（Static Forward Edges）

遍历所有 `Reference`，通过 `enclosing_map` 将 source/target 解析到最近的节点符号：

- **Call** → 添加 `Call` edge（通过 `init_map` 回退解析构造函数）；未解析的引用收集到 `unresolved_calls`
- **Read** → 添加 `Read` edge
- **Write** → 添加 `Write` edge
- **Decorate** → 添加 `Annotates` edge
- 收集 `call_assignments`（variable → external call target）供 Pass 2.5 使用

#### Pass 2.5: Type Propagation

- 从 `Definition.details` 填充节点的类型引用（`return_types`, `param_type`, `var_type`）
- **External Call Return Type Propagation**：对于 `call_assignments` 中的变量，如果调用目标是外部函数且有 return type，将 return type 传播到被赋值的变量

#### Pass 3: ImplementedBy Edges + Edge Recovery

- **OverriddenBy**：对每个属于某类型的方法节点，通过 `TypeRegistry.implementors`（现在包含所有子类型）查找子类型，匹配同名方法，添加 `parent_method → child_method` 边。统一处理 interface implementation 和 concrete override 两种关系
- **Type-Driven Call Edge Recovery**：对 `unresolved_calls`，通过 receiver 变量的传播类型解析目标方法，添加恢复的 `Call` 边。迭代至不动点（每轮至少恢复 1 条边或终止）

> **注意**：不再在 Pass 3 中构建 `SharedStateWrite` 和 `CallIn` 边。这些反向探索逻辑移至 Part 1 的遍历算法中，在运行时通过 `graph.incoming_edges()` 实现。这简化了图构建过程，同时将遍历策略集中在 Solver 中。
> Builder 假定 Extractor 输出已经满足 correctness invariants。对于 `target_symbol=None` 的 unresolved
> 引用，Builder 只做保守恢复，不负责修正错误 owner、伪类型、或 fabricated external symbols。
> 

### 2.3 Builder Pseudocode

```
function build_graph(semantic_data, source_reader, size_fn, doc_scorer):
    graph = empty_graph()
    type_registry = empty_registry()
    enclosing_map = semantic_data.build_enclosing_map()
    node_symbols = empty_set()

    // ─── PASS 1: Nodes + TypeRegistry ───
    for each document in semantic_data.documents:
        source_code = source_reader.read(document.path)
        for each def in document.definitions:
            context_size = compute_context_size(def, source_code, size_fn)
            doc_score = doc_scorer.score(def)

            if def.kind == Type:
                type_registry.register(def.symbol_id, def, context_size, doc_score)
                for iface in def.details.implements:
                    type_registry.register_implementor(iface, def.symbol_id)
                for base in def.details.inherits:
                    type_registry.register_implementor(base, def.symbol_id)

            else:  // Function or Variable
                node = create_node(def, context_size, doc_score)
                graph.add_node(def.symbol_id, node)
                node_symbols.add(def.symbol_id)

    init_map = build_init_map(semantic_data, node_symbols, type_registry)

    // ─── PASS 2: Static Forward Edges ───
    call_assignments = {}
    unresolved_calls = []

    for each document in semantic_data.documents:
        for each ref in document.references:
            source_sym = resolve_to_node(ref.enclosing_symbol, node_symbols, enclosing_map)
            target_sym = resolve_to_node(ref.target_symbol, node_symbols, enclosing_map)
            if source_sym is None: continue

            source_idx = graph.index(source_sym)

            match ref.role:
                Call:
                    target_idx = graph.index(target_sym) OR graph.index(init_map[ref.target_symbol])
                    if target_idx exists:
                        graph.add_edge(source_idx, target_idx, Call)
                    else if target_sym is None:
                        unresolved_calls.append(ref)
                    if ref.assigned_to is not None:
                        call_assignments[ref.assigned_to] = { caller: source_idx, target: target_sym }

                Read:
                    target_idx = graph.index(target_sym)
                    if target_idx exists:
                        graph.add_edge(source_idx, target_idx, Read)

                Write:
                    target_idx = graph.index(target_sym)
                    if target_idx exists:
                        graph.add_edge(source_idx, target_idx, Write)

                Decorate:
                    target_idx = graph.index(target_sym)
                    if target_idx exists:
                        graph.add_edge(source_idx, target_idx, Annotates)

    // ─── PASS 2.5: Type Propagation ───
    fill_type_references(graph, semantic_data, type_registry)
    propagate_external_return_types(graph, call_assignments, type_registry)

    // ─── PASS 3: OverriddenBy + Edge Recovery ───
    build_overridden_by_edges(graph, type_registry)
    recover_call_edges_to_fixpoint(graph, unresolved_calls, type_registry)

    graph.type_registry = type_registry
    return graph
```

---

## Part 3: Python Semantic Data Extractor

实现 Python 语言特定的 `SemanticData` 提取器，输出符合 Semantic Data Schema 的 JSON。

### 3.1 Extraction Responsibilities

Extractor 需要从 Python 源码中提取：

1. **Definitions**：函数（含方法、构造函数）、变量（模块级、类字段）、类型（class, protocol, enum 等）
2. **References**：函数调用（Call）、变量读写（Read/Write）、装饰器（Decorate）
3. **Type information**：参数类型、返回类型、变量类型、继承/实现关系
4. **Symbol hierarchy**：`enclosing_symbol` 链（用于 symbol resolution）

### 3.2 Implementation Approach

基于 Python AST 或 LSP（如 Pyright/Pylance）实现：

- 使用 AST 解析获取定义和引用的 span、kind、enclosing 关系
- 使用 LSP 的 type inference 获取类型信息（`param_type`, `return_types`, `var_type`）
- 处理 Python 特有的模式：
    - `__init__` 方法识别（用于构造函数调用解析）
    - Protocol / ABC 识别（用于 `is_interface_method` 标记）
    - `@property` / `@dataclass` / `@frozen` 等装饰器的语义识别（影响 mutability 判断）
    - Module-level `__all__` 和 `_` 前缀的 visibility 推断
    - TypeVar 的 bound/constraints 提取

实现时还必须满足 `docs/extractor-spec.md` 中的行为约束，特别是：

- 链式访问必须基于中间表达式的值类型，而不是仅看最左侧变量
- resolver 查询必须命中语义 referent token，而不是整个表达式起点
- `Any`、`Unknown`、`Self@...`、原始签名文本等非信息型类型不能直接拿来拼 symbol
- 如果 Python 的静态信息不足，应优先输出 unresolved，而不是生成看似合法但实际错误的 target

相比 Rust、Java、强类型 TypeScript 等语言，Python extractor 通常需要更多 fallback 和 normalization。
但这种额外复杂度属于语言特性，不改变 Part 2 的数据契约，也不改变 Part 1/Part 2 的语义。

### 3.3 Output

Extractor 输出标准的 `SemanticData` JSON，供 Part 2 的 Builder 直接消费。不同 Python 版本或工具链（AST vs LSP）可以有不同的 Extractor 实现，只要输出符合 Schema。
