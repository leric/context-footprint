# ADR-005: Separate CF traversal from boundary evaluation

## Status

Accepted. Supersedes ADR-003.

## Context

The original implementation encoded pruning directly in the domain solver through
`PruningParams`, `evaluate_forward`, and `should_explore_callers`. This mixed two
questions:

1. which reasoning dependency follows from a program fact;
2. whether a particular contract surface is reliable enough to stop traversal.

The revised CF definition also makes traversal direction- and mode-sensitive. The
same function can be reached as callee behavior, caller usage evidence, mutable-state
provenance, or an override obligation. A fixed edge-only predicate cannot represent
those obligations without moving traversal semantics into the heuristic.

## Decision

The domain owns the fixed CF traversal rules:

- CF-in and CF-out directions;
- reasoning modes and their permitted transitions;
- caller, mutable-state, decorator, and override relation derivation;
- fragment reachability, de-duplication, overlap, and truncation semantics.

A domain `BoundaryPolicy` port owns only boundary reliability:

```text
evaluate(mode, relation, source, target, type_registry)
    -> StopAtSurface | EnterImplementation | Unknown
```

`Unknown` is treated conservatively as `EnterImplementation`.

The existing Academic and Strict configurations are retained as parameters of a
`SyntacticBoundaryPolicy`. Alternative static, LLM-based, or trace-calibrated
policies may be added without changing CF traversal.

Every result records `boundary_policy_id`; policy parameters are therefore part of
the measurement provenance.

## Consequences

- Solver tests can fix boundary decisions independently from traversal tests.
- Heuristic experiments no longer redefine CF itself.
- Legacy heuristics must be explicitly assigned to a concrete policy rather than
  silently embedded in traversal.
- The domain remains independent of adapters: `BoundaryPolicy` is a domain port,
  and policy implementations consume only domain data.
