# Context Footprint: Measuring Structural Context Depth in Software

## Abstract

Coupling metrics in software engineering predominantly count dependency edges, treating all dependencies as equally costly to reason about. This paper presents **Context Footprint (CF)**, a static metric that instead estimates the volume of program context a reader must load to understand a code unit's structural dependencies and obligations. CF operationalizes the **depth** axis of the **Context Minimization Principle (CMP)** [@cmp]. 

CF separates structural depth into two directions. **CF-out** measures the context required to understand what a unit does: callees, state, decorators, dynamic implementations, and other behavior behind its outgoing dependencies. **CF-in** measures the context required to understand how a unit is used: callers, usages, overrides, and other external obligations that become relevant when the unit's own contract is insufficient. Total CF is the deduplicated union of both footprints.

The metric is defined independently from the heuristics used to estimate boundary reliability. A CF traversal pays the boundary surface of each encountered artifact. If a configurable **Boundary Policy** judges that surface sufficient for local reasoning, traversal stops; otherwise the implementation is loaded and reasoning dependencies continue to expand. This separation keeps the metric stable while allowing boundary estimation, graph precision, and context-size functions to evolve independently.

CF is not an absolute score of design quality; its strongest interpretation is comparative, between designs that carry the same responsibility. We evaluate CF on a natural experiment of exactly that kind: the large-scale, agent-driven refactor of Nous Research's Hermes Agent, which reduced the codebase by 34% with the stated goal of making it easier for agents to work with. Comparing the two frozen versions, we ask whether CF captures information that size metrics do not, where the remaining footprint comes from, and whether CF predicts the context coding agents actually acquire. **[Placeholder: key findings.]**

## 1. Introduction

Coupling has long been recognized as a central factor affecting software maintainability, modifiability, and comprehension. Dominant coupling metrics such as CBO, MPC, and Ca/Ce quantify coupling primarily by counting dependency edges. This treats each dependency as a uniform unit of cost: a call to a narrow, well-specified abstraction and a call into a large, behaviorally opaque subsystem both contribute one edge.

A growing body of work, most notably the Vovel metrics [2021], challenges this uniform-edge assumption by measuring the amount of information transferred through dependencies. That move is important, but it still leaves out a familiar engineering experience: some dependencies can be reasoned about entirely from their visible contract, while others force the reader to keep traversing implementation, state, configuration, dynamic dispatch, and usage sites before the behavior becomes clear.

The companion paper _Context Minimization as a Lens for Software Design_ [@cmp] frames this problem in terms of **context cost**: correct modification requires sufficient context, and software design determines how expensive that context is to acquire. CMP identifies two recurring shapes of context expansion:

- **Depth**: how far a modifier must traverse structural dependencies before behavior or obligations become sufficiently understood.
- **Breadth**: how hard it is to acquire the complete modification closure—the set of artifacts that must be considered together for one concrete change to be correct.

The distinction matters for measurement. Breadth is inherently **modification-relative**: the same function may participate in very different closures under different changes. Depth, by contrast, can be treated as a **structural property of the artifact** under a fixed static-analysis and boundary policy: starting from this unit, how much surrounding program context must be loaded before its dependencies and external obligations can be reasoned about locally?

This paper operationalizes that structural property as **Context Footprint (CF)**.

> **Context Footprint measures the structural context depth of a code unit.**
>
> **CF-out measures the context required to understand what the unit depends on.**
>
> **CF-in measures the context required to understand what depends on, or constrains, the unit.**

The distinction between CF-in and CF-out makes explicit a symmetry that ordinary coupling metrics blur. A caller may need to traverse inward because a callee's boundary does not explain enough behavior. An implementer may need to traverse outward through callers because the unit's own contract does not explain what external code relies on. Both are failures of local reasoning, but in opposite directions.

CF differs from traditional coupling measures in three ways. First, it measures **context volume rather than edge count**. Second, it treats abstraction boundaries as **conditional cut points**: a dependency contributes only its visible surface when that surface is sufficient, but exposes implementation context when it is not. Third, it is **transitive**: reasoning continues through dependencies until reliable stopping points are reached.

The metric is deliberately narrower than CMP. It does not attempt to recover modification closure, evaluate task-specific breadth, or assign an absolute quality score to a design: a high CF may reflect essential problem complexity rather than poor structure. Its strongest design use is therefore **controlled comparison**—the same responsibility, reorganized—where a reduction in CF can be read directly as a reduction in structural context cost (Section 4).

This makes large refactorings the natural evaluation setting, and we evaluate CF on one. In 2026 Nous Research refactored its Hermes Agent codebase using 1,393 coding agents, reducing non-test Python from 1.06 million to 698 thousand lines, with the explicit goal of making the code easier for agents to work with. Two frozen versions of one real project, transformed with a stated intent, give us what a controlled comparison needs. We ask three questions: whether CF changes in ways that size metrics cannot explain, in particular whether *code decomposition* (smaller functions) coincides with *context decomposition* (smaller footprints); where the footprint that remains comes from; and whether CF predicts the context coding agents actually acquire when working on both versions.

### Contributions

This paper makes four contributions:

1. **A static metric for structural context depth.** We define CF as the deduplicated union of two directional footprints, CF-out and CF-in, computed over a language-independent program fact graph.
2. **A separation between metric definition and boundary estimation.** CF defines what is measured; a configurable Boundary Policy estimates where traversal may safely stop. Type annotations, documentation, immutability, executable contracts, or LLM-based judgments are implementation strategies rather than part of the metric definition.
3. **An implementation architecture for computable CF.** We describe a three-stage pipeline—language-specific semantic extraction, language-independent graph construction, and state-aware CF traversal—with explicit extension points for graph precision, boundary policy, and context-size functions.
4. **A natural-experiment evaluation.** Using the two versions of the Hermes Agent refactor, we test whether CF separates code decomposition from context decomposition, attribute residual footprints to specific boundaries, and test whether CF predicts agent context acquisition, with ablations over boundary policy and traversal defaults.

Together, these contributions turn the intuition that “not all dependencies expose the same amount of context” into a concrete metric with a deliberately bounded interpretation.

## 2. Background: Depth Without Breadth

### 2.1 CMP Context Cost

CMP defines context cost as the cost of acquiring sufficient context for correct modification [@cmp]. The full principle is modification-relative because sufficiency depends on what change is being made. For one feature, a schema, validator, UI field, test fixture, and analytics query may all belong to the required context; for another change, only one of them may matter.

That task-level variation is primarily a **breadth** problem. Breadth asks which artifacts belong to the modification closure and whether that closure can be acquired completely.

CF does not attempt to solve that problem.

Instead, CF isolates the other recurring source of context cost: **depth**. Once a code unit has entered the working set, how much additional structural context does a reader need to load before the unit can be understood locally? The structural dependencies surrounding a unit—calls, reads, writes, decorators, overrides, dynamic implementations, and usage constraints—exist before a particular maintenance task is chosen. Their exact relevance to a modification may vary, but they define a stable structural exposure that can be estimated statically.

This gives CF a clean scope boundary:

> **Breadth selects which artifacts a modification must consider. CF prices the structural depth of an artifact once it is selected.**

A real modification pays both, but they are not the same measurement problem.

### 2.2 Two Directions of Depth

Depth expands in two directions.

**Outward depth** appears when understanding a unit requires opening what it depends on. A function calls another function whose signature does not explain failure semantics; that callee must be opened. The callee reads mutable shared state; the writers of that state must be inspected. A call targets an abstract method whose contract is too weak; concrete implementations must be examined.

**Inward depth** appears when understanding a unit's obligations requires opening what depends on it. A function accepts untyped values and documents no preconditions; its callers become evidence for what inputs it is expected to handle. A base method has a weak contract; subclasses and usages reveal assumptions the implementation must preserve. A public function can only be changed safely after inspecting how its result is interpreted elsewhere.

These directions motivate the two components of CF:

\[
CF_{out}(u)
\]

for dependency-facing context, and

\[
CF_{in}(u)
\]

for usage- and obligation-facing context.

The total footprint is the deduplicated context required by both directions:

\[
CF(u)=Size(R_{out}(u) \cup R_{in}(u))
\]

where \(R_{out}(u)\) and \(R_{in}(u)\) are sets of source-context fragments reached by each traversal. Conceptually the total is “CF-out plus CF-in,” but implementations must avoid double-counting fragments reachable from both sides.

### 2.3 Reliable Boundaries as Compression Points

A dependency does not necessarily expose its implementation. A well-designed boundary can compress a large internal body into a much smaller reasoning surface: a name, signature, type contract, documented semantics, error model, effect declaration, or other externally visible specification.

CF therefore distinguishes two costs for an artifact \(v\):

- \(surface(v)\): the context needed to understand the visible contract of \(v\);
- \(implementation(v)\): the internal context required only when the surface is insufficient.

Traversal always pays the relevant surface. It pays the implementation only when the boundary does not provide a trustworthy stopping point.

This leads to the central operational behavior of CF:

```text
reliable boundary:
    pay surface
    stop

transparent / under-specified boundary:
    pay surface + implementation
    continue traversal
```

The distinction matters more than raw indirection. A program may contain many interfaces and still have high CF if none of them let the reader stop. Conversely, a program may contain several layers while remaining locally understandable if each layer exposes a sufficient contract.

## 3. Context-Footprint Metric

### 3.1 Overview

CF is computed in three conceptual stages:

1. **Extract program facts** from source code: definitions, references, types, ownership, and external symbols.
2. **Build a language-independent Program Fact Graph** containing functions, variables, and structural relationships.
3. **Traverse reasoning dependencies** from a target unit in the CF-out and CF-in directions, stopping at boundaries judged sufficient by a Boundary Policy.

The implementation is intentionally split this way so that language parsing, structural graph construction, and reasoning policy do not leak into one another.

```text
Semantic Data Schema              Graph Schema
       ↓                              ↓
[Extractor]  →  [Graph Builder]  →  [CF Query]
(language-specific)  (language-agnostic)  (metric / traversal)
```

The Graph Builder records what relationships exist in the program. The CF Query layer decides which of those relationships must be expanded for reasoning.

### 3.2 Program Fact Graph

We model a software system as a directed graph

\[
G_p=(V,E)
\]

where nodes represent **functions/methods** and **variables**. Type definitions are maintained in a separate **Type Registry** and referenced by node attributes.

We define five program-fact edge kinds:

| Edge kind | Direction | Meaning |
|---|---|---|
| `Call` | Function → Function | source may invoke target |
| `Read` | Function → Variable | source may read target state |
| `Write` | Function → Variable | source may modify target state |
| `OverriddenBy` | Parent Method → Child Method | child overrides or implements parent method |
| `Annotates` | Decorated → Decorator | decorator may alter decorated behavior |

These edges are **program facts**, not direct claims that the target must always be read. CF derives reasoning dependencies from them according to traversal direction and state.

The graph should conservatively over-approximate relationships relevant to structural reasoning. Different implementations may use different analysis precision—lexical resolution, class-hierarchy analysis, LSP type information, points-to analysis, or compiler semantic models. Higher precision reduces false edges; false negatives are more dangerous because they can make CF spuriously low.

### 3.3 Reasoning Dependencies

A **Reasoning Dependency** means:

> To satisfy the current reasoning obligation for artifact \(a\), artifact \(b\) may need to enter the working context.

Reasoning dependencies need not have the same direction as program edges.

For example, a normal call produces an outward reasoning dependency:

```text
caller --Call--> callee
caller --needs behavior of--> callee
```

Mutable state produces a derived dependency through incoming writes:

```text
writer_1 --Write--> S <--Write-- writer_2
                         ↑
                       Read
                         |
                       reader
```

To understand what `reader` may observe, CF-out may need the declaration of `S` and the behavior of its possible writers. The graph contains only ordinary `Read` and `Write` facts; the reasoning traversal derives the provenance relation when needed.

Likewise, an under-specified function creates an inward reasoning dependency from the function to its callers:

```text
caller_1 --Call--> f <--Call-- caller_2

f --usage evidence--> caller_1
f --usage evidence--> caller_2
```

A distinction between “forward traversal” and special “reverse exploration” is therefore unnecessary at the metric level. Both are instances of state-dependent reasoning traversal over the same program facts: the current **reasoning mode** determines which program edges are followed, in which direction, and what is charged (Section 3.9).

### 3.4 CF-out: Dependency Footprint

`CF-out(u)` answers:

> What structural context must be loaded to understand what \(u\) depends on?

Common expansions include:

**Calls.** For `u --Call--> v`, the traversal first loads `surface(v)`. If the surface is sufficient, traversal stops. Otherwise it loads `implementation(v)` and continues from `v`.

**Immutable state.** For `u --Read--> S`, if `S` is constant or immutable, its declaration, type, and initializer are sufficient by construction. No writer search is needed; this is decided by the program fact, not by the Boundary Policy.

**Mutable shared state.** If `S` is mutable, understanding its possible values requires following incoming `Write` edges to its writers. Each writer `w` is charged as a full behavioral dependency: its implementation is loaded and its own dependencies continue to expand. Because the written value may also originate from `w`'s parameters, `w`'s callers become usage evidence for `w` as well (Section 3.9, D2). All of this remains part of the outward footprint of the original reader.

**Writes.** For `u --Write--> S`, the outward direction needs only `surface(S)`: the declaration, type, and mutability of the target. Writing does not require knowing other writers or readers; readers are an inward obligation (Section 3.5).

**Decorators and hooks.** A decorator, middleware layer, descriptor, lifecycle hook, or other explicitly modeled mechanism that can alter behavior belongs to CF-out when its semantics are needed to understand the target.

**Dynamic dispatch.** When a call resolves to an abstract or interface method, a sufficient contract stops traversal at the abstract surface. An insufficient contract causes the analysis to expand through `OverriddenBy` relations to possible implementations.

CF-out therefore measures transitive implementation exposure, not merely outgoing degree.

### 3.5 CF-in: Obligation and Usage Footprint

`CF-in(u)` answers:

> What structural context must be loaded to understand how \(u\) is used, depended on, or constrained from outside?

A reliable contract can make CF-in small. If the unit itself clearly expresses accepted inputs, outputs, preconditions, side effects, failure semantics, and invariants relevant to callers, an implementer can reason from that contract without scanning usage sites.

If the unit is under-specified, incoming relationships become evidence. The analysis inspects callers to infer real input shapes, error assumptions, ordering expectations, or result semantics. Concretely, CF-in expands as follows.

**Contract gate.** Whether any usage evidence is loaded at all is decided once per unit by asking the Boundary Policy about the unit's *own* contract. If the contract is sufficient, no callers are loaded. This is the same question the outward direction asks about a callee; only the subject changes from target to source.

**Callers.** For each `c --Call--> u`, both `surface(c)` and `implementation(c)` are charged: the call site lives in the body, and a signature alone does not reveal what `c` passes or how it interprets the result. Whether traversal continues to `c`'s callers is decided by `c`'s own contract gate. A caller with a complete contract already constrains the values it forwards; an under-specified caller may merely pass its own unannotated parameters through, so its callers become further evidence. Crucially, a caller reached as usage evidence does **not** expand its own callees: `c`'s other dependencies are irrelevant to `u`'s obligations.

**Overrides.** Inheritance contributes in two directions. If `u` overrides a parent method `p`, `surface(p)` is always charged—it is a contract `u` must honor—and, when `u`'s gate is open, callers of `p` are also callers of `u` through dynamic dispatch. If `u` is itself overridden, its overriders are charged (surface and implementation) as evidence of what the method family is expected to do, without further expansion.

**Readers of written state.** The inward analogue of Section 3.4's mutable-state rule: if `u` writes a mutable shared variable `S`, the readers of `S` depend on the value `u` writes. When `u`'s gate is open, `surface(S)` and each reader's surface and implementation are charged, without further expansion.

This is the inward analogue of information hiding. A good boundary does not only protect callers from implementation details; it protects implementers from caller-specific assumptions.

### 3.6 Boundary Policy

Whether a boundary is truly sufficient is semantic and cannot be decided perfectly by syntax alone. CF therefore separates the **metric definition** from the **boundary estimator**.

A Boundary Policy answers one question: *is this artifact's contract sufficient to support local reasoning under the current reasoning mode?* It receives the reasoning state and a candidate relation and returns one of three decisions:

```text
enum BoundaryDecision {
    Boundary,      // contract sufficient: do not expand further
    Transparent,   // contract insufficient: continue expansion
    Unknown,
}
```

Conceptually:

```text
BoundaryPolicy.evaluate(
    mode,          // current reasoning mode (Section 3.9)
    relation,      // kind of reasoning relation being followed
    subject,       // the artifact whose contract is judged
    context,       // the other endpoint, for reference only
    type_registry,
) -> BoundaryDecision
```

`Unknown` is treated as `Transparent` under the conservative default and recorded in the result as an unresolved relation, so that the share of the footprint attributable to uncertain judgments remains visible.

Two conventions keep the policy's role narrow. First, the decision governs only **whether traversal continues**, never **what is charged**: the fragments charged on encountering a relation are fixed by the transition rules in Section 3.9 (outward relations charge the target's surface; usage evidence charges surface and implementation because the call site is in the body). Second, the **subject differs by direction**: outward modes judge the *target* (“does what I depend on explain itself?”), while the inward mode judges the *current unit itself* (“do I explain myself, or must others' usage be read?”). Both directions ask the same question of different artifacts.

Some decisions are fixed by program facts and bypass the policy entirely: reading a constant or immutable variable always stops, and external targets always stop (Section 3.8).

A simple implementation may use syntactic evidence such as:

- complete parameter and return types;
- documentation coverage;
- abstract/interface status;
- immutability;
- explicit error or effect representation.

More sophisticated implementations may use executable contracts, tests, LLM-based semantic evaluation, or calibration against observed agent traces. These choices affect the estimator, not the definition of CF itself.

This distinction is essential. “CF measures structural context volume” should remain stable even if the community later finds a much better way to decide whether a boundary is trustworthy.

### 3.7 Surface and Implementation Size

Each internal artifact is represented by at least two context fragments:

\[
surface(v)
\]

and

\[
implementation(v)
\]

For a function, the surface may include its name, visibility, parameters, types, return type, contract-relevant decorators, documentation, error model, and effect information. The implementation includes the body and internal control/data flow that need not be loaded when the surface is sufficient.

For a variable, the surface may include its name, type, mutability, declaration-level documentation, and initializer when the initializer defines the value's semantics.

The default size function is the LLM token count of each context fragment, because that directly corresponds to prompt/context consumption for an agent. Alternative functions—lexical tokens, AST nodes, normalized tokens—are permissible as long as they are applied consistently.

This split fixes an important distortion in node-level size models. Reaching a reliable 20-token interface in front of a 2,000-token implementation should cost approximately the interface surface, not the implementation body. The entire purpose of an abstraction is that the hidden body no longer belongs to the required context for ordinary reasoning.

**Types referenced by a surface are part of it.** A signature such as `f(cfg: Config) -> Result` is small, but if `Config` is an 80-field structure, understanding `f`'s contract requires understanding `Config`. Whenever `surface(v)` is charged, the surface fragment of every repository-local type referenced in `v`'s signature (parameters, return type, or variable type) is charged as well, deduplicated across the traversal. A type surface consists of its declaration, field and method signatures, and documentation, but not method bodies; methods are separate function nodes that enter the traversal only when called. Types are not traversal nodes and derive no further relations. Without this rule, a boundary could hide its cost inside a type and CF would be systematically underestimated.

### 3.8 External Dependencies as Measurement Boundaries

External dependencies are treated as **measurement boundaries by scope**, not as evidence of good design.

For an external dependency:

```text
pay external API surface
stop
```

CF measures repository- or workspace-local structural context. The implementation of a third-party dependency lies outside that measurement universe unless the analysis scope explicitly includes it. External types referenced in signatures are treated the same way: their analyzer-visible surface is charged, their definition is not entered.

In practice the visible surface of an external symbol is often little more than its name, unless stubs or documentation are available to the analyzer. This makes external calls nearly free and biases CF toward designs that push logic into libraries; the evaluation reports sensitivity to the external-surface size function.

This avoids the stronger and unnecessary assumption that third-party APIs are always well specified. An external API may be awkward or poorly documented; CF simply does not recursively price source code outside its declared scope.

### 3.9 State-Aware Traversal and Transition Rules

A simple `visited: Set<NodeId>` is insufficient because the same node may be reached under different reasoning obligations. A function reached as a callee needs implementation analysis; the same function reached as usage evidence needs caller-side analysis.

Traversal state is therefore a pair:

```text
TraversalState { node_id, mode }
```

Three reasoning modes suffice:

| Mode | Applies to | Question answered | Typical origin |
|---|---|---|---|
| `Behavior` | Function | What does this function do? | Outward root; transparent callee, decorator, implementor, or writer |
| `Provenance` | Variable | What values may this mutable state hold? | `Read` of a mutable variable |
| `Usage` | Function | How is this function used or constrained from outside? | Inward root; under-specified caller or parent method |

Direction (In/Out) is a property of the traversal—which root state it started from—not of the state. An outward traversal may legitimately contain `Usage` states (a writer's callers, D2 below) and they still belong to \(R_{out}\).

State deduplication and context-cost deduplication are separate:

```text
visited_states:    Set<(NodeId, Mode)>
reached_fragments: Set<ContextFragmentId>
```

The analyzer may revisit an artifact under a different mode, but the same source fragment is counted only once. Each traversal keeps its own `reached_fragments`; the two directions do not share a paid set, otherwise fragments charged first by CF-out would be missing from CF-in.

**Transition rules.** The complete semantics of the traversal are given by two tables: what a state charges when processed (Table A, below), and which relations a state derives, what each charges on encounter, who decides whether it continues, and which state it pushes (Table B, Appendix A). Everything else in the algorithm is bookkeeping.

*Table A — what a state charges when processed.*

| State | Charges |
|---|---|
| `(f, Behavior)` | `surface(f)` + `implementation(f)` |
| `(S, Provenance)` | `surface(S)` |
| `(f, Usage)` | `surface(f)` + `implementation(f)` |

A state is only enqueued once it has been decided that its implementation is needed, so Table A is unconditional. Variables have no separate implementation fragment; their initializer belongs to the surface.

Table B formalizes the expansions described in Sections 3.4 and 3.5. In the outward direction it follows outgoing `Call`, `Read`, `Write`, `Annotates`, and `OverriddenBy` edges from a `Behavior` state, and incoming `Write` edges from a `Provenance` state. In the inward direction, all usage evidence is gated by the unit's own contract, and a `Usage` state never derives outgoing edges. The root states are `(u, Behavior)` for \(R_{out}(u)\) and `(u, Usage)` for \(R_{in}(u)\).

Six choices in Table B are not forced by the metric definition—among them whether signature types are charged, how far inward traversal follows callers, and how a writer reached through mutable state is expanded. We fix defaults, label them D1–D6 in Appendix A, and treat the alternatives as ablations in Section 5. Three of them (writer expansion, multi-hop callers, readers of written state) are the main sources of large footprints; they are also what makes shared mutable state and under-specified APIs expensive under CF.

### 3.10 Formal Computation

Let \(R_{out}(u)\) be the set of context fragments reached by outward traversal and \(R_{in}(u)\) the set reached by inward traversal under a fixed program graph, Boundary Policy, size function, and measurement scope.

Then:

\[
CF_{out}(u)=Size(R_{out}(u))
\]

\[
CF_{in}(u)=Size(R_{in}(u))
\]

and

\[
CF(u)=Size(R_{out}(u) \cup R_{in}(u))
\]

with

\[
Size(R)=\sum_{f \in R} size(f)
\]

for deduplicated context fragments \(f\).

The overlap

\[
CF_{overlap}(u)=Size(R_{out}(u) \cap R_{in}(u))
\]

may be reported for diagnostics. Conceptually the total footprint combines both directions; the union prevents cycles or bidirectional relationships from charging the same source twice.

The target unit's own surface and implementation are the common base of both directions: they belong to both \(R_{out}(u)\) and \(R_{in}(u)\) and are counted once in the union. Consequently \(CF_{out}\) and \(CF_{in}\) each include the unit itself, and \(CF_{overlap}(u) \geq size(u)\).

\(R_{out}\) and \(R_{in}\) are produced by two independent traversals. Resource limits such as `max_tokens`, `max_nodes`, or timeouts are execution controls, not part of the metric, and apply to each direction separately. A truncated analysis must report a lower bound such as `CF >= 128k` together with which direction was truncated, not treat the cutoff as the true score.

The solver is a breadth-first traversal over states that performs the table lookups and nothing else (pseudocode in Appendix B). Besides the fragment sets it records which relations were cut by a `Boundary` decision and which rested on an `Unknown` one. This makes a footprint explainable—which boundaries held, which were transparent, and how much depends on uncertain judgments—and is what allows a CF difference between two designs to be attributed to specific boundaries rather than reported as a single scalar. Section 5 relies on this for attribution.

### 3.11 Implementation Extension Points

CF keeps four implementation choices explicit:

1. **Size function.** LLM tokens, lexical tokens, AST nodes, or another stable proxy.
2. **Boundary Policy.** The estimator used to decide whether a surface is sufficient.
3. **Graph construction precision.** The static-analysis method used to recover calls, writes, overrides, and dynamic targets.
4. **Measurement scope.** Which repository, workspace, packages, or generated artifacts belong to local context.

These choices, together with the documented defaults D1–D6 of Appendix A, influence absolute values and conservativeness. Any reported CF value is therefore a value of a metric family \(CF_{G,B,\sigma,S}\) indexed by graph construction, Boundary Policy, size function, and scope; empirical comparisons must hold all four constant across conditions.

## 4. Interpretation: What CF Can and Cannot Say

CF makes a descriptive statement about structural context exposure:

> Given this code version and analysis configuration, approximately this much program context is structurally exposed when reasoning about the target unit.

It does **not** follow that higher CF always means worse design. Consider two functions:

```text
A: 1,000 lines of inherently complex algorithmic logic, almost no dependencies
B: 50 lines of orchestration, plus 950 lines of exposed dependency context
```

If both have CF near 1,000 tokens, the metric should report similar reasoning volume. It should not invent a design verdict that the measurement does not support: `A` may simply encode a harder problem.

Two kinds of comparison are nevertheless legitimate, and it is important to keep them apart.

**Comparing burden across units** is always meaningful as a descriptive statement: `A` requires more structural context than `B` under the analyzer. This is what a predictive-validity study tests when it asks whether CF tracks the context an agent actually acquires. It says nothing about which unit is better designed.

**Comparing designs** requires holding the responsibility approximately constant:

\[
\Delta CF = CF_{after} - CF_{before}
\]

For a behavior-preserving refactoring, or for alternative implementations of the same contract, a negative \(\Delta CF\) means less structural context is required to understand the same responsibility. This is the claim a context-oriented design metric should support, and it is the setting of our evaluation.

Three further limits bound the interpretation. First, CF is not a formal verification footprint and makes no soundness claim about program semantics; “sufficient boundary” is an engineering judgment approximated by static evidence. Second, CF does not subsume cyclomatic complexity, nesting, runtime frequency, correctness, performance, or business importance; these are independent dimensions. Third, and most important, CF measures depth, not breadth: it only sees context that is *connected* to the target through program facts. The Hermes refactor (Section 5) provides a concrete illustration. During review, the team found that several public names had been removed because they had no callers inside the repository—but they were imported by external plugins. From the local dependency graph the cleanup looked correct, and the CF of the affected units was unremarkable. The information needed for a correct change existed, but nowhere the traversal could reach. A large footprint is a visible cost: the reader keeps opening files because nothing lets them stop. Disconnected context is an invisible one: the reader feels finished. CF measures the first kind only.

## 5. Empirical Evaluation

### 5.1 Setting: A Natural Experiment

Evaluating a comparative metric requires two designs of the same thing. Synthetic refactorings are easy to construct but unconvincing; real refactorings at scale are rare and rarely frozen. The Hermes Agent refactor is an unusual opportunity.

Hermes Agent is an open-source coding-agent framework maintained by Nous Research. In 2026 the team refactored it using 1,393 coding agents working in parallel, with the explicitly stated goal of making the codebase easier for agents to work with. Non-test Python fell from 1.06 million to 698 thousand lines (−34.4%); files over 5,000 lines from 37 to 6; functions over 300 lines from 192 to 2. Both versions are frozen at public commits **[Placeholder: commit hashes]**. The team also published its own measurement: across the same 4,000 symbols, the average code returned per symbol lookup fell from 2,218 to 993 tokens, while noting that the split increased module count and import dependencies, so that some coupling likely remained.

This gives us the ingredients a controlled comparison needs: one real project, two versions, a transformation with stated intent, and an independent size-based measurement to triangulate against. It also gives us a sharp question that size metrics cannot answer. Splitting a 5,000-line function into twenty 250-line functions decomposes the *code*; whether it decomposes the *context* required to reason about the behavior depends on whether the new boundaries let a reader stop. We call the first **code decomposition** and the second **context decomposition**.

The refactor is not behavior-preserving in the strict sense—a third of the code was removed—so we do not treat the whole-repository comparison as a controlled one. Instead we use the repository-level shift descriptively (RQ1) and restrict the controlled comparison to matched functions whose responsibility is plausibly unchanged (RQ2).

### 5.2 Research Questions

- **RQ1 (Distributional shift).** How does the distribution of CF over non-test functions change across the refactor, and how does the change compare with size-based metrics (definition size, LOC, symbol lookup cost, CBO)?
- **RQ2 (Code vs. context decomposition).** For functions present in both versions, does the change in CF carry information beyond the change in definition size? How often does a large reduction in size coincide with little or no reduction in CF?
- **RQ3 (Attribution).** Where does the residual footprint of poorly decomposed functions come from: CF-out or CF-in, transparent boundaries, dynamic dispatch, or shared mutable state? Do the boundaries the policy judges transparent agree with independent human or LLM judgment?
- **RQ4 (Construct validity).** When coding agents perform the same tasks on both versions with oracle localization, does the change in CF of the target units predict the change in context the agents acquire?
- **RQ5 (Sensitivity).** How robust are RQ1–RQ2 to the Boundary Policy, the traversal defaults D1–D6, the size function, and the treatment of external surfaces?

### 5.3 Measurement

**Analyzer configuration.** We compute CF with the reference implementation described in Section 3 and Appendix A: Python semantic extraction via **[Placeholder: extractor]**, LLM-tokenizer size function (**[Placeholder: tokenizer]**), repository scope excluding tests and vendored code, and the syntactic Boundary Policy of Section 3.6 with `doc_threshold` = **[Placeholder]**. The same configuration is applied to both versions. Any truncated result is reported as a lower bound and excluded from ratio statistics.

**Baselines.** For each function we also record definition size in tokens, LOC, CBO at the enclosing class or module, and the number of reachable files. Where available, we align with Nous Research's symbol lookup cost.

**Function matching (RQ2).** Functions are matched across versions by fully qualified name; unmatched functions are re-matched through rename and move detection **[Placeholder: tool, e.g. RefactoringMiner-style heuristics]** and body similarity. We retain only pairs whose signature is unchanged or changed by annotation only, and report the retained fraction. Functions that were split are represented by the surviving entry point; functions that were removed or introduced are excluded from RQ2 and reported separately.

**Boundary judgment (RQ3).** From the `boundary_stops` and transparent-boundary records of the functions with the smallest CF reductions, we sample **[Placeholder: N]** boundaries and ask two independent judges (**[Placeholder: human and/or LLM protocol]**) whether the surface alone suffices to reason about the call without opening the implementation. We report agreement with the policy.

**Agent tasks (RQ4).** We select **[Placeholder: N]** maintenance tasks that are expressible against both versions (issues fixed after the refactor whose fix touches units present in both). Agents (**[Placeholder: agent and model]**) receive oracle localization—the target unit—so that the measured cost is depth rather than search. We record tokens read through file and search tools, distinct files opened, and task success, over **[Placeholder: k]** runs per task and version. We also compute fragment-level overlap: the fraction of \(R(u)\) actually read by the agent (recall) and the fraction of agent-read code inside \(R(u)\) (precision).

### 5.4 Analysis

**RQ1.** Percentiles (median, P90, P99, max) and mean of CF before and after, alongside the same statistics for definition size and reachable files. Structural counts (files, definitions, edges) characterize the shape of the change.

**RQ2.** For matched pairs, the relationship between \(\Delta \log size\) and \(\Delta \log CF\): correlation, partial correlation controlling for the change in reachable files, and the share of variance in \(\Delta CF\) unexplained by \(\Delta size\). We partition pairs into quadrants and report the fraction with a large size reduction (**[Placeholder: threshold]**) but a CF reduction below **[Placeholder]**—code decomposition without context decomposition—and the converse.

**RQ3.** For the functions in that quadrant, the decomposition of residual CF into CF-out, CF-in, and overlap; the distribution of transparent boundaries by reason (missing types, missing documentation, interface method, abstract dispatch, mutable state); and policy–judge agreement.

**RQ4.** Paired comparison of agent context volume before and after per task, and the relationship between per-task \(\Delta CF\) of the target unit and \(\Delta\) tokens read, with task and run as random effects. Fragment overlap is reported as precision/recall distributions.

**RQ5.** RQ1 and RQ2 recomputed under: the two extreme policies `always-stop` (footprint equals direct dependencies) and `never-stop` (full transitive closure), which bracket the contribution of conditional boundaries; a stricter syntactic policy; each of D1–D6 toggled; an AST-node size function; and external surfaces charged as signatures from stubs instead of names.

### 5.5 Results

**[Placeholder: results for RQ1–RQ5. Pilot measurements with an earlier version of the analyzer indicated a modest median reduction, a much larger reduction at P99 and at the maximum, and matched pairs at both extremes—entry points whose size and CF fell together by roughly 90%, and orchestrators whose size fell by over 90% while CF fell by only a few percent and reachable files increased. Final numbers to be recomputed with the analyzer of Section 3.]**

### 5.6 Threats to Validity

**Construct.** CF depends on the Boundary Policy, and the syntactic policy is a coarse proxy for contract sufficiency. RQ3 and RQ5 measure the size of this dependence rather than assuming it away. Definition size and CF are not independent—the unit's own implementation is part of its footprint—so RQ2 controls for size explicitly.

**Internal.** The refactor removed code as well as reorganizing it; matched-pair filtering reduces but does not eliminate responsibility drift. Rename detection may mismatch functions; we report the retained fraction and the sensitivity of RQ2 to stricter matching. Python type-annotation coverage differs between versions, which affects the policy directly; we report annotation coverage for both.

**External.** One project, one language, one refactoring team (of agents). Hermes is unusually large and unusually agent-oriented; results may not transfer to human-driven refactors or to statically typed languages, where the syntactic policy behaves differently. The agent study (RQ4) uses a small number of tasks and a single agent configuration.

**Conclusion validity.** Agent runs are stochastic; we use repeated runs and paired analysis. The analyzer was built by the authors, who also selected the tasks; task selection criteria are stated in advance and the analyzer configuration is fixed before RQ4 is run.

## 6. Related Work

### 6.1 Coupling Metrics: From Edge Counting to Context Volume

The dominant coupling metrics in software engineering research share a common characteristic: they primarily count dependency relationships without distinguishing how much additional context each dependency exposes.

**Edge-counting metrics.** Chidamber and Kemerer's **Coupling Between Objects (CBO)** counts distinct classes to which a class is coupled. Li and Henry's **Message Passing Coupling (MPC)** counts outgoing method invocations. Robert Martin's **Afferent/Efferent Coupling (Ca/Ce)** counts package-level dependencies. To such metrics, a dependency on a small stable interface and a dependency on a large opaque implementation may contribute the same unit.

**Dynamic and weighted coupling.** Dynamic coupling measures based on runtime traces capture actual rather than merely potential interactions. Frequency-weighted approaches distinguish common from rare edges, but they weight by execution usage rather than by the amount of context required for reasoning.

**Semantic and evolutionary coupling.** Information-retrieval methods measure conceptual similarity, while version-control mining reveals logical coupling through files that often change together. The latter is especially relevant to CMP's breadth axis: co-change history can reveal modification relationships that are not obvious in the structural graph. CF deliberately does not absorb that task/evolution dimension.

**Information-flow metrics.** Henry and Kafura proposed an information-flow complexity metric:

\[
Complexity = Length \times (Fan\text{-}in \times Fan\text{-}out)^2
\]

This moves beyond raw edge count toward information-throughput intuition, but it still does not model conditional stopping at abstraction boundaries or transitive implementation exposure.

The gap addressed by CF is narrower:

> How much repository-local source context is structurally exposed when reasoning about a code unit before reliable boundaries let the reader stop?

### 6.2 Vovel Metrics

The **Vovel metrics** (Vovel-in and Vovel-out) are CF's closest empirical relatives because they explicitly combine coupling with information volume.

Vovel estimates information transferred through method signatures by summing the sizes of parameters and return types. This validates the intuition that dependency edges should not all be treated as equal.

CF differs in three central respects:

1. **Reasoning context rather than transferred data.** CF includes context that may need to be inspected even when no corresponding data volume crosses the method boundary.
2. **Transitive traversal.** CF follows reasoning dependencies beyond the immediate edge when the next boundary is insufficient.
3. **Conditional boundary compression.** A reliable contract contributes its surface without exposing its implementation; an unreliable one expands the footprint.

The directional vocabulary also differs. Vovel-in/out describe information-flow coupling, whereas CF-in/out describe two directions of structural reasoning burden: usage obligations versus dependencies.

### 6.3 Information Hiding and Local Reasoning

CF's cut-point behavior is grounded in the same engineering intuition as information hiding: implementation details should cease to be required context once a client reaches a trustworthy abstraction. Parnas's work on modular decomposition and Ousterhout's account of dependencies and obscurity provide the design background for this view [Parnas, 1972; Ousterhout, 2018].

The analogy to separation logic and formal local reasoning is useful but limited. CF is not a proof system and does not inherit formal frame properties. It uses local reasoning as an engineering criterion: if a boundary surface is sufficient for ordinary reasoning, hidden implementation should not be charged to the footprint.

### 6.4 Relationship to CMP

CF operationalizes one part of CMP, not all of it. CMP's full context cost includes structural depth, modification breadth, discoverability, implicit knowledge, and confidence in completeness; CF isolates the first because it is the part that can be computed statically from the artifact alone (Section 2.1). A maintenance task can then be modeled in two stages: breadth determines which artifacts enter the working set; CF prices the structural depth of each. The full CMP cost is not the sum of CF scores, because breadth includes the discovery and completeness costs that CF does not measure—the Hermes plugin regression in Section 4 is a case in point.

### 6.5 Summary

| Metric family | What it measures | CF's distinction |
|---|---|---|
| CBO / MPC / Ca-Ce | number of dependency edges | weights by exposed context volume |
| Dynamic coupling | runtime interaction frequency | measures reasoning exposure, not frequency |
| Semantic coupling | conceptual similarity | measures structural reasoning dependencies |
| Evolutionary coupling | co-change history | belongs mainly to breadth/change-together analysis |
| Henry-Kafura | fan-in/fan-out intensity | models boundary stopping and transitive exposure |
| **Vovel** | parameter/return information volume | adds transitive traversal + boundary cut points |

CF complements these measures by making one familiar design effect measurable: a successful boundary can turn a large implementation into a small reasoning surface.

## 7. Conclusion

Context Footprint measures one structural property: how much program context remains exposed when reasoning outward through a code unit's dependencies and inward through its external obligations, before reliable boundaries let the reader stop. The property is decomposed into **CF-out** and **CF-in**, computed by a state-aware traversal whose semantics are fixed by two small tables, and separated from the heuristics that estimate boundary sufficiency so that policy, graph precision, and size function can improve without changing what CF means.

The Hermes refactor shows why such a metric is worth having. Size metrics report that the codebase became smaller and its functions shorter; they cannot say whether the context needed to reason about any given behavior shrank with it. CF can, and it can say where the remaining context comes from. **[Placeholder: one-paragraph summary of findings—distributional shift, the code-vs-context decomposition split among matched functions, attribution of residual footprints, and the agent study.]**

We do not claim that CF scores design. We claim that when the same responsibility is reorganized, a reduction in CF expresses what the reorganization was supposed to achieve: the next reader—human or agent—understands the same behavior while loading less of the surrounding system. As coding agents take on more maintenance work, and as agent-driven refactors of the Hermes kind become common, that is a property worth measuring directly rather than inferring from line counts.

## Appendix A. Transition Rules and Documented Defaults

This appendix gives Table B in full: the relations derived from each traversal state (Section 3.9). “On encounter” is charged unconditionally before any decision. `policy(x)` invokes the Boundary Policy with subject `x`; `fact` means the decision follows from program facts; `—` means the relation never continues. External targets charge their surface and stop without consulting the policy (Section 3.8). `surface(v)` always includes the surfaces of repository-local types referenced in `v`'s signature (Section 3.7).

### A.1 Outward, from `(f, Behavior)`, along `f`'s outgoing edges

| Program fact | On encounter | Decision | If Transparent, push |
|---|---|---|---|
| `f --Call--> g` | `surface(g)` | `policy(g)` | `(g, Behavior)` |
| `f --Read--> S`, `S` immutable | `surface(S)` | `fact` → Boundary | — |
| `f --Read--> S`, `S` mutable | `surface(S)` | `fact` → Transparent | `(S, Provenance)` |
| `f --Write--> S` | `surface(S)` | — | — |
| `f --Annotates--> d` | `surface(d)` | `policy(d)` | `(d, Behavior)` |
| `f --OverriddenBy--> f_i` | `surface(f_i)` | `policy(f_i)` | `(f_i, Behavior)` |

The `OverriddenBy` row fires only when `f` is already in a `Behavior` state, i.e. the abstract method's own contract was judged insufficient. Each implementor is judged independently, since a subclass may carry a more complete contract than its parent.

### A.2 Outward, from `(S, Provenance)`, along `S`'s incoming writes

| Program fact | On encounter | Decision | If Transparent, push |
|---|---|---|---|
| `w --Write--> S` | `surface(w)` | `policy(w)`, default Transparent | `(w, Behavior)` and `(w, Usage)` |

Understanding what `w` writes requires `w`'s behavior (the value may come from a callee) and `w`'s inputs (the value may come from a parameter, hence from a caller). The latter is `(w, Usage)`, which is gated by `w`'s own contract as in A.3.

### A.3 Inward, from `(f, Usage)`

All rows marked *gated* are derived only if `policy(f)`—the unit's own contract—returns Transparent; the gate is evaluated once per unit.

| Program fact | Gate | On encounter | Decision | If Transparent, push |
|---|---|---|---|---|
| `p --OverriddenBy--> f` (`f` overrides `p`) | no | `surface(p)` | — | — |
| `p --OverriddenBy--> f` | gated | — | `fact` → Transparent | `(p, Usage)` |
| `c --Call--> f` | gated | `surface(c)` + `implementation(c)` | `fact` → Transparent | `(c, Usage)` |
| `f --OverriddenBy--> f_i` | gated | `surface(f_i)` + `implementation(f_i)` | — | — |
| `f --Write--> S`, `S` mutable, `r --Read--> S` | gated | `surface(S)` + `surface(r)` + `implementation(r)` | — | — |

The caller row's decision is `fact` rather than `policy(c)`: whether to continue past `c` is decided by `c`'s own gate when `(c, Usage)` is processed, so the policy answers exactly one question per function. A `Usage` state never derives outgoing edges. There is no `Annotates` row in `Usage` mode: decorators alter `f`'s behavior, not others' obligations toward it.

### A.4 Documented defaults

| ID | Choice | Default | Alternative |
|---|---|---|---|
| D1 | Charge types referenced in signatures | yes (Section 3.7) | type names only |
| D2 | Expansion after reaching a writer | `Behavior` and `Usage` | `Behavior` only; or implementation without expansion |
| D3 | Multi-hop CF-in | yes, gated by each caller's contract | one hop |
| D4 | Continue past overriders reached as evidence | no | push `(f_i, Usage)` |
| D5 | Charge readers of state the unit writes | yes, one hop | no |
| D6 | `Unknown` policy decision | Transparent | Boundary (lower-bound estimate) |

## Appendix B. Solver Pseudocode

```text
function compute_cf(graph, u, policy, size, limits):
    out = traverse(graph, (u, Behavior), policy, limits)
    in  = traverse(graph, (u, Usage),    policy, limits)

    return {
        cf_out:        size(out.fragments),
        cf_in:         size(in.fragments),
        cf_total:      size(out.fragments UNION in.fragments),
        cf_overlap:    size(out.fragments INTERSECT in.fragments),
        truncated_out: out.truncated,
        truncated_in:  in.truncated,
        boundary_stops: out.stops UNION in.stops,
        unresolved:    out.unresolved UNION in.unresolved,
    }
```

The core traversal contains no relation-specific logic; `charge`, `derive_relations`, and the pushed states are lookups into Table A (Section 3.9) and Table B (Appendix A):

```text
function traverse(graph, root, policy, limits):
    queue = [root]
    visited = set()          // (NodeId, Mode)
    reached = set()          // ContextFragmentId
    stops = []; unresolved = []

    while queue not empty:
        if limits exceeded:
            return { fragments: reached, truncated: true, stops, unresolved }

        state = queue.pop_front()
        if state in visited: continue
        visited.add(state)

        charge(state, reached)                       // Table A

        for rel in derive_relations(graph, policy, state):   // Table B, incl. Usage gate
            charge_on_encounter(rel, reached)        // surface(s), incl. referenced types
            if rel.target.is_external: continue      // measurement boundary

            decision = rel.decision
            if decision is PolicyCall:
                decision = policy.evaluate(state.mode, rel.kind, rel.subject, rel.other, registry)
                if decision == Unknown:
                    unresolved.append(rel); decision = Transparent

            if decision == Boundary:
                stops.append(rel); continue
            for next in rel.next:                    // possibly several (D2)
                queue.push_back(next)

    return { fragments: reached, truncated: false, stops, unresolved }
```

Each traversal keeps its own `reached` set; the two directions are combined only in `compute_cf`. Sharing a paid set between them would omit from \(R_{in}\) any fragment first charged by the outward traversal.
