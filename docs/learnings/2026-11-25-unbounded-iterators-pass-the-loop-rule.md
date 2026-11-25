# Unbounded iterators pass the bounded-loop rule

- Status: Open
- Found in: the guardrail review of 2026-11-25

## What happened

Tidy's `unbounded-loop` rule reports `loop`, `while` and a `for` over a range without an end.
It does not know the other ways to iterate without a bound:

```rust
for value in core::iter::repeat(1.0) { .. }
for value in values.iter().cycle() { .. }
for step in core::iter::successors(Some(0_u32), |step| Some(step + 1)) { .. }
```

## Proposed guardrail

Tidy, in `unbounded-loop`: report a `for` whose iterable chain calls `repeat`, `repeat_with`,
`cycle`, `from_fn` or `successors`, unless a `take` or `zip` follows. Clippy's
`maybe_infinite_iter` is in the pedantic group and catches some adapter chains, so check first
what it already reports.
