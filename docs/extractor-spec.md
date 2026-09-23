# Extractor Specification

> Formal contract for language-specific extractors. Any extractor that outputs `SemanticData`
> conforming to this spec can be consumed by the language-agnostic Builder + Solver.

## Overview

An extractor converts source code into `SemanticData` (defined in `src/domain/semantic.rs`).
The JSON schema defines the output shape. This document defines the correctness contract:

1. **Reference coverage**: which semantic situations MUST produce which references.
2. **Correctness invariants**: which classes of wrong output are forbidden, even if the
   extractor has partial information.
3. **Language mapping**: how a language-specific extractor should realize the above.
4. **Conformance tests**: how an implementation proves that it satisfies the contract.

A new extractor is not considered complete merely because it emits "some plausible graph".
It must:

- cover every applicable abstract reference pattern,
- satisfy every applicable correctness invariant,
- and have a passing conformance test for each required case.

## Three Layers of This Spec

1. **Abstract Reference Patterns**: language-agnostic dependency situations. This is the "what".
2. **Correctness Invariants**: cross-language rules that prevent systematically wrong output.
   This is the "what must never go wrong".
3. **Language Mapping + Conformance Tests**: AST mapping, resolution strategy, and fixtures.
   This is the "how" and the "proof".

---

## Part 1: Abstract Reference Patterns

Each pattern describes a semantic situation where one symbol depends on another symbol.
AST forms differ by language, but the dependency meaning is the same.

### Call Patterns

| ID | Pattern | Role | Description |
|----|---------|------|-------------|
| REF-CALL-01 | Direct function call | Call | `foo()` |
| REF-CALL-02 | Method call on enclosing instance/class | Call | `self.method()` / `this.method()` |
| REF-CALL-03 | Method call on typed receiver | Call | `obj.method()` where `obj` has a known value type |
| REF-CALL-04 | Method call on untyped receiver | Call | `obj.method()` where the receiver type is unknown; emit unresolved |
| REF-CALL-05 | Constructor call | Call | `MyClass()` / `new MyClass()` |
| REF-CALL-06 | Super/parent method call | Call | `super().method()` / `super.method()` |
| REF-CALL-07 | Class method factory call | Call | `cls()` / `cls.method()` inside classmethod-style context |
| REF-CALL-08 | Decorator / annotation application | Decorate | `@decorator` |

### Read Patterns

| ID | Pattern | Role | Description |
|----|---------|------|-------------|
| REF-READ-01 | Read module/global variable | Read | `print(CONFIG)` |
| REF-READ-02 | Read instance/class field | Read | `self.field` / `obj.field` |
| REF-READ-03 | Default argument value | Read | `def f(x=SENTINEL)` |
| REF-READ-04 | Function/method as value | Read | `callback = handler` / `register(fn)` |
| REF-READ-05 | Exception type in catch | Read | `except MyError` / `catch (MyError e)` |

### Write Patterns

| ID | Pattern | Role | Description |
|----|---------|------|-------------|
| REF-WRITE-01 | Assign to module/global variable | Write | `CONFIG = val` |
| REF-WRITE-02 | Assign to instance/class field | Write | `self.field = val` / `obj.field = val` |
| REF-WRITE-03 | Compound assignment | Read + Write | `x += 1` |
| REF-WRITE-04 | Delete variable/field | Write | `del obj.field` |

### Notes

- Local variables are excluded as graph nodes. They may carry type information for resolution,
  but they do not become graph nodes and should not create edges by themselves.
- Type annotations are metadata, not edges. Their role is to support signature completeness
  and type-driven recovery.
- The patterns above apply recursively to arbitrary receiver expressions, not only simple names.
  `obj.method()`, `a.b.method()`, and `factory().method()` are all instances of the same
  abstract receiver-call pattern.
- When a target cannot be resolved with acceptable confidence, emit the reference as unresolved
  (`target_symbol=None`) and preserve partial information such as `method_name` and `receiver`
  when available.

### Function Fragment Ranges

Function definitions must populate two language-neutral range lists:

- `surface_spans`: signature, contract-relevant decorators/modifiers, documentation, and
  explicit error/effect declarations needed without entering the body;
- `implementation_spans`: body ranges not already represented by the surface.

Ranges must not overlap. Together they need not cover whitespace or comments that carry no
contract or behavioral information. When an older extractor omits both lists, the Builder
uses a compatibility fallback based on the full definition span and normalized signature
metadata; this fallback is less precise and should be reported through the size-function
provenance.

---

## Part 2: Cross-Language Correctness Invariants

Reference coverage is not enough. An extractor can "cover" a pattern and still emit incorrect
symbols that poison the graph. The following invariants are cross-language and mandatory.

### Invariant Table

| ID | Invariant | Requirement |
|----|-----------|-------------|
| INV-SYM-01 | Stable symbol identity | The same semantic entity must have exactly one stable `symbol_id` across definitions and references |
| INV-SYM-02 | No fabricated pseudo-symbols | Extractors MUST NOT synthesize symbols from placeholders such as `Any`, `Unknown`, `Self@...`, raw signatures, or similar analyzer artifacts |
| INV-RES-01 | Query the referent token | Resolver queries must target the token that denotes the semantic referent, not merely the enclosing expression start |
| INV-TYPE-01 | Receiver means value type | For `expr.member`, type-driven resolution must use the value type of `expr`, not a broader container or owner type |
| INV-TYPE-02 | Conservative over wrong | If the best available type information is non-informative or ambiguous, the extractor MUST leave the reference unresolved rather than invent a target |
| INV-SCOPE-01 | Inference uses syntactic scope | Local/field value-type inference must use the true enclosing definition from syntax/AST, not transient traversal state |
| INV-EXT-01 | External symbols must be semantically real | External symbols may be materialized only when they denote a stable library/module/type/member, not a placeholder type root |
| INV-ATTR-01 | Chains are first-class | `a.b.c()` and `a.b.c = x` must be resolved using the intermediate value `a.b`, not reduced to `a` |

### Practical Meaning of the Invariants

#### INV-SYM-01: Stable Symbol Identity

If a definition pass emits `pkg.mod.outer`, the reference pass must not emit
`pkg.mod.outer.inner` for the same extracted node merely because the syntax was nested.
Any instability in `symbol_id` breaks graph construction.

#### INV-SYM-02: No Fabricated Pseudo-Symbols

The following are examples of forbidden output roots:

- `Any.upper`
- `Unknown.foo`
- `Self@__init__.add`
- `def get_db.executescript`

These strings may appear in analyzer hover output, but they are not valid graph symbols.

#### INV-RES-01: Query the Referent Token

For a member chain such as `app.config.from_mapping()`, the resolver query for the call target
must point at `from_mapping`, and the query for the intermediate field type must point at
`config`. Querying the start of the whole expression (`app`) is incorrect.

This invariant generalizes to all languages with member access syntax.

#### INV-TYPE-01 and INV-ATTR-01: Receiver Means Value Type

For `a.b.c()`, the call target depends on the value type of `a.b`, not just the declared type
of `a`. Likewise for writes such as `a.b.c = x`, the write target is the field on the value type
of `a.b`.

#### INV-TYPE-02: Conservative Over Wrong

If the extractor cannot confidently infer the receiver value type, unresolved output is correct.
A missing edge is preferable to a confidently wrong edge with a fake symbol.

#### INV-SCOPE-01: Inference Uses Syntactic Scope

Offline inference helpers must not depend on runtime visitor stacks that are only valid during
one traversal. When inferring types from local assignments or field initializers, the enclosing
definition must be reconstructed from syntax/AST or other stable metadata.

#### INV-EXT-01: External Symbols Must Be Semantically Real

Materializing `pathlib.Path.read_text` or `werkzeug.routing.Map.add` is correct.
Materializing `Literal.upper` or `Any.upper` is not.

### Recommended Decision Rule

When in doubt, apply this order:

1. Emit the correct internal or external symbol.
2. Otherwise emit unresolved with partial information.
3. Never emit a fabricated pseudo-symbol.

### Strongly Typed vs Weakly Typed Languages

The invariants above are universal, but the implementation burden is not.

#### Strongly Typed Languages

Examples: Rust, TypeScript (in typed codebases), Java, Kotlin, C#.

These languages usually make extractors simpler because:

- receiver value types are often directly available from the compiler or language service,
- member targets are usually exposed as stable symbols rather than free-form hover text,
- field and local variable types are more often explicit,
- unresolved cases are concentrated at dynamic boundaries rather than ordinary method calls.

For these languages, extractor implementations should prefer:

- structured symbol/type APIs from the compiler or language server,
- direct symbol identity mapping over string parsing,
- minimal heuristics,
- unresolved output only at genuinely dynamic boundaries.

Typical risk areas still remain:

- aliasing and re-exports,
- trait/interface dispatch,
- macros/code generation,
- generic instantiation boundaries,
- overloaded or extension methods,
- partially typed TypeScript code (`any`, dynamic property access).

#### Weakly Typed or Gradually Typed Languages

Examples: Python, Ruby, JavaScript, mixed-quality TypeScript.

These languages usually require more fallback logic because:

- receiver types may be implicit or unavailable,
- local assignments often carry the only usable type clue,
- analyzer outputs may contain placeholders or textual approximations,
- many ordinary member calls sit near the boundary between statically knowable and dynamic.

For these languages, the extractor must invest more in:

- receiver value-type inference,
- import-context disambiguation,
- type-text normalization and pseudo-symbol filtering,
- negative tests that prove the extractor does not fabricate targets.

#### Important Constraint

Strong typing reduces the number of heuristics, but it does not relax the invariants.

Even for Rust or TypeScript, an extractor is still incorrect if it:

- emits unstable `symbol_id`s,
- queries the wrong token in a member chain,
- collapses `a.b.c()` to the type of `a`,
- or fabricates external symbols from analyzer placeholders.

---

## Part 3: Python Language Mapping

This section maps the abstract patterns and invariants to Python AST forms and resolution rules.
It is intentionally phrased in backend-agnostic terms. The current implementation may use `ty`,
Jedi, or another resolver, but the behavioral contract is the same.

### Definitions (Pass 1)

| What | Python AST | Scope Rule |
|------|------------|------------|
| Module-level function | `FunctionDef` / `AsyncFunctionDef` with no class enclosing | `module.func_name` |
| Class method | `FunctionDef` inside `ClassDef` | `module.Class.method` |
| Class definition | `ClassDef` | kind=`Type`, inherits from bases |
| Module-level variable | `Assign` / `AnnAssign` with no function enclosing | kind=`Variable`, scope=`Global` |
| Class body field | `Assign` / `AnnAssign` in class body | kind=`Variable`, scope=`Field` |
| Instance field | assignment target `self.x` inside method | kind=`Variable`, scope=`Field` |
| Nested function | `FunctionDef` inside function | NOT extracted as a standalone symbol; references are attributed to the nearest extracted enclosing definition |

### References (Pass 2)

| Pattern ID | Python AST Form | Resolution Strategy |
|------------|-----------------|---------------------|
| REF-CALL-01 | `Call(func=Name('foo'))` | Resolver definition lookup on callee token; fallback by import context |
| REF-CALL-02 | `Call(func=Attribute(value=Name('self'/'cls'), attr='m'))` | Enclosing class + method name; resolver may refine |
| REF-CALL-03 | `Call(func=Attribute(value=<expr>, attr='m'))` | Resolve member token first; if needed infer value type of `<expr>` and map to `Type.m` |
| REF-CALL-04 | Same as above with no informative receiver type | Emit unresolved with `method_name='m'` and `receiver` when available |
| REF-CALL-05 | `Call(func=Name('MyClass'))` | Resolve class/type; builder maps via constructor convention to `__init__` |
| REF-CALL-06 | `Call(func=Attribute(value=Call(func=Name('super')), attr='m'))` | Resolve parent method from enclosing class bases, including cross-module and alias cases |
| REF-CALL-07 | `Call(func=Name('cls'))` or `Attribute(value=Name('cls'), ...)` | Treat `cls()` as constructor and `cls.m()` as method on enclosing class |
| REF-CALL-08 | `decorator_list` entries | Resolve decorator expression and emit `Decorate` |
| REF-READ-01 | `Name(ctx=Load)` resolving to module/class variable | Emit `Read` |
| REF-READ-02 | `Attribute(ctx=Load)` resolving to field/module variable | Resolve attribute token or infer receiver value type; emit `Read` |
| REF-READ-03 | `args.defaults`, `args.kw_defaults` | Walk default expression and resolve symbols within it |
| REF-READ-04 | `Name/Attribute(ctx=Load)` resolving to function/method used as value | Emit `Read` |
| REF-READ-05 | `ExceptHandler.type` | Resolve type expression and emit `Read` |
| REF-WRITE-01 | `Name(ctx=Store)` resolving to variable | Emit `Write` |
| REF-WRITE-02 | `Attribute(ctx=Store)` resolving to field | Resolve attribute token or infer receiver value type; emit `Write` |
| REF-WRITE-03 | `AugAssign(target=Name/Attribute)` | Resolve target and emit both `Read` and `Write` |
| REF-WRITE-04 | `Delete(targets=[Name/Attribute])` | Resolve target and emit `Write` |

### Python Resolution Ladder

For each reference, try resolution in this order:

1. **Exact resolver query on the referent token**
   - For `foo()`, query `foo`.
   - For `obj.method()`, query `method`.
   - For `obj.field`, query `field`.
2. **Definition matching**
   - Match resolver result to internal definitions by file + line + name.
   - If internal match fails and the result is a real external symbol, materialize it.
3. **Value-type inference**
   - Infer the value type of the receiver expression, not merely the leftmost name.
   - Use, in order:
     - declared parameter type,
     - declared variable/field type,
     - local assignments preceding the use,
     - function return types,
     - resolver hover/type info on the receiver expression,
     - import context.
4. **Pattern-specific fallback**
   - `self.method()` / `cls.method()` via enclosing class.
   - `super().method()` via enclosing class bases.
   - default-arg name disambiguation via import context.
5. **Conservative unresolved**
   - If the remaining type information is non-informative, emit unresolved.

### Python Normalization Rules

Python extractors MUST apply these normalization rules before synthesizing a target symbol:

- Query `Attribute` nodes at the member token, not the expression start.
- Normalize literal-derived types:
  - `Literal["x"]` -> `builtins.str`
  - `Literal[1]` -> `builtins.int`
  - `LiteralString` -> `builtins.str`
- Treat these as non-informative and unsuitable for symbol synthesis:
  - `Any`, `typing.Any`, `t.Any`
  - `Unknown`
  - raw signature text such as `def foo() -> ...`
  - self-like placeholders such as `Self@...`
- If the best candidate is builtins noise and the project does not track builtins as graph nodes,
  prefer dropping the target to emitting a fake external symbol.

### Python-Specific Clarifications

- Nested functions are not graph nodes. References inside nested functions are attributed to the
  nearest extracted enclosing definition.
- Chained attribute access is not a special case; it is the normal behavior of receiver value-type
  inference applied recursively.
- External symbol materialization is valid only after a symbol root is normalized to a semantically
  real module/type/member path.

---

## Part 4: Conformance Test Suite

Each language extractor must provide fixtures that prove both coverage and correctness.

### Test Structure

Each test:

1. creates a fixture exercising one pattern or invariant,
2. runs the extractor,
3. asserts the expected `(enclosing_symbol, role, target_symbol)` or equivalent negative property.

### Python Conformance Tests

| Requirement | Fixture(s) | Assertion |
|-------------|------------|-----------|
| REF-CALL-01 | `simple.py` | Call from `foo` to `bar` |
| REF-CALL-02 | `test_self_call.py` | Call from `APIRouter.put` to `APIRouter.api_route` |
| REF-CALL-03 | `test_method_resolve.py` | Call from `create_image_edit` to `RelayImageUseCase.execute` |
| REF-CALL-03 + INV-ATTR-01 | `sample.py` chained field call fixture | `app.config.from_mapping()` resolves to `Config.from_mapping`, not owner/container type |
| REF-WRITE-02 + INV-ATTR-01 | `sample.py` chained field write fixture | `self.inner.value = 1` resolves to `Inner.value` |
| REF-CALL-05 | builder/integration fixture | Constructor call maps to `__init__` |
| REF-CALL-06 | `test_super_call.py` | Same-module `super().__init__` resolves to base constructor |
| REF-CALL-06 | `base_super.py`, `child_super.py` | Cross-module `super().__init__` resolves correctly |
| REF-CALL-06 | `base_alias.py`, `child_alias.py` | Aliased base class still resolves correctly |
| REF-CALL-07 | `test_cls_call.py` | `cls(name)` resolves to enclosing class `__init__` |
| REF-CALL-08 | `test_annotated_doc.py` | Decorator extraction works |
| REF-READ-03 | `test_default_arg_ref.py` | Same-module default arg read resolves correctly |
| REF-READ-03 | `sentinel_def.py`, `default_arg_cross.py` | Cross-module default arg read resolves correctly |
| REF-READ-03 | `sentinel_a.py`, `sentinel_b.py`, `default_arg_ambig.py` | Import context disambiguates same-named defs |
| REF-READ-04 | `test_func_as_value.py` | Function used as value emits `Read` |
| REF-READ-05 | `test_except_resolve.py` | Exception type emits `Read` |
| REF-WRITE-03 | `test_augassign.py` | AugAssign emits both `Read` and `Write` |
| INV-SYM-01 | nested-function fixture | Nested helper does not become a standalone definition; references remain attributed to outer function |
| INV-SCOPE-01 | constructor-context fixture | Field initialized in `__init__` carries the correct inferred value type into later method calls |
| INV-EXT-01 | external-method fixture | A correctly inferred external method target is materialized into `external_symbols` |
| INV-SYM-02 | unknown-return fixture | No pseudo-symbol like `def get_db.executescript` is emitted |
| INV-SYM-02 | any/literal receiver fixtures | No pseudo-symbol like `Any.upper` or `Literal.upper` is emitted |

### Conformance Philosophy

The test suite must contain both:

- **positive assertions**: the correct edge exists,
- **negative assertions**: known bad pseudo-symbols are absent.

This is important. Many extractor failures are not "missing edge" failures; they are
"wrong symbol but still graph-shaped" failures. Negative tests are therefore part of the
contract, not merely optional hygiene.

### Adding a New Language

When implementing an extractor for a new language:

1. create `extractors/<language>/`,
2. copy the abstract pattern table and invariant table,
3. add a language mapping section equivalent to Part 3,
4. create one fixture per applicable pattern/invariant,
5. implement until all tests pass,
6. do not mark the extractor complete until both positive and negative cases pass.

Patterns that do not apply to a language should be marked `N/A` in that language mapping.

### Implementation Checklist

Use this checklist before declaring a new extractor "complete".

#### Phase 1: Symbol Model

- Define the language's symbol naming scheme and ensure it is stable across passes.
- Verify that every extracted definition kind maps cleanly to `Function`, `Variable`, or `Type`.
- Decide which language constructs are graph nodes and which are only metadata.
- Write tests proving nested/local-only constructs do not accidentally become graph nodes when the language model says they should not.

#### Phase 2: Reference Coverage

- Implement every applicable abstract reference pattern from Part 1.
- For each pattern, add at least one positive conformance fixture.
- Ensure unresolved output preserves partial information (`method_name`, `receiver`) where the schema supports it.

#### Phase 3: Resolver Discipline

- Verify that every resolver query is issued on the semantic referent token, not a larger enclosing expression.
- Verify that internal symbol matching is based on stable metadata such as file, line, symbol identity, or compiler symbol id.
- If the language tooling returns textual type information, define a normalization step before any target synthesis.

#### Phase 4: Type-Driven Resolution

- Implement receiver value-type inference for member reads/calls/writes.
- Prove that chained access uses the intermediate expression value type (`a.b.c()` via `a.b`).
- Ensure value-type inference can use syntactic scope without relying on transient traversal state.
- Define which type roots are considered informative versus placeholders/noise.

#### Phase 5: External Symbols

- Define when an external symbol may be materialized.
- Prove that real external APIs are materialized when confidently known.
- Prove that placeholder roots and pseudo-types are not materialized.

#### Phase 6: Negative Correctness Tests

- Add tests that assert the absence of known bad pseudo-symbols.
- Add tests for wrong-owner regression classes, not only missing-edge cases.
- Add tests for symbol-id stability across definition and reference passes.
- Add at least one conservative-unresolved case where the correct behavior is to avoid guessing.

#### Phase 7: End-to-End Validation

- Run the extractor on a medium-sized real project in that language.
- Inspect several representative functions with low, medium, and high context footprint.
- Verify that suspicious high-CF results come from real dependency structure, not pseudo-symbols or wrong-owner edges.
- Compare graph statistics before and after fixes, but do not use density alone as a correctness signal.

### Acceptance Checklist

An extractor should only be considered ready when all of the following are true:

- all applicable conformance tests pass,
- all required negative tests pass,
- no known pseudo-symbol class is present in real-project output,
- unresolved references correspond to genuine information gaps rather than avoidable mis-resolution,
- and a real-project spot check finds no major invariant violations.

---

## Appendix: Patterns Explicitly Out of Scope

These patterns are fundamentally dynamic and should not be "solved" with brittle heuristics.
When encountered, prefer unresolved output with partial information.

| Pattern | Example | Why Out of Scope |
|---------|---------|------------------|
| Dynamic attribute access | `getattr(obj, name)` | Attribute name is runtime data |
| Metaclass-generated methods | `class Meta(type): ...` | Methods created during class creation |
| Monkey-patching | `obj.method = lambda: ...` | Runtime replacement of behavior |
| `exec` / `eval` generated symbols | `exec("def foo(): ...")` | Code generated from strings |
| Star imports | `from mod import *` | Namespace depends on external module state |
| Descriptor protocol | `__get__`, `__set__` | Implicit dispatch through runtime protocol |
| `__getattr__` / `__getattribute__` | dynamic fallback attribute resolution | Intercepts nearly all attribute access |

When an extractor encounters these patterns, it should emit unresolved references with whatever
partial information is available, rather than synthesizing unstable or fake targets.
