# The float-sum lint sees `sum` and an adding `fold` only

- Status: Open
- Found in: the guardrail review of 2026-11-25, while writing `nami_float_reduction`

## What happened

`nami_float_reduction` reports `Iterator::sum` with a float result and a `fold` whose closure
adds to a float accumulator. Scratch runs showed that these naive reductions pass:

```rust
values.iter().product::<f64>()
values.iter().copied().reduce(|a, b| a + b)
values.iter().fold(0.0, |total, value| total - value)
values.iter().copied().fold(0.0, f64::add)
values.iter().map(|&value| Some(value)).sum::<Option<f64>>()
```

A sum written as a `for` loop with `+=` passes too. That one cannot be told apart from other
accumulations and stays with review.

## Proposed guardrail

Dylint, in `lints/src/float_reduction.rs`: also report `product`, `reduce`, `try_fold` and
`rfold` with a float accumulator, a fold that subtracts, and `sum` into `Option<f64>` or
`Result<f64, _>`. Each needs a firing and a non-firing UI case; a max or min reduction must
stay silent.
