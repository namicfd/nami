# ADR-0029: Interface, output and determinism guarantees

- Status: Proposed
- Date: 2026-11-26
- Deciders: maintainer

## Context

Users are researchers and their agents (ADR-0022), and they write problem crates (ADR-0025).
ADR-0001 rules out `toml`, `serde`, HDF5 and `pyo3`, and deferred the choice of output format to
a later ADR, which is this one. ADR-0005 and ADR-0027 make results bitwise reproducible. Users
can rely on that only if every output says how it was made and a restart can tell when the build
has changed.

## Decision

1. **No outside standard.** The interface and file formats optimise for nami's goals.
   Compatibility with existing tools is kept where it comes for free.
2. **A problem crate is a binary** with the commands `run <config>`, `check <config>` and
   `restart <checkpoint>`, and `--set key=value` overrides.
3. **The configuration is a strict subset of TOML,** read by nami's own parser. It has no
   defaults. Unknown keys, missing keys and wrong types are errors whose message names the file,
   the line and the fix.
4. **Diagnostics:** per-step JSON Lines (time, time step, conservation totals, activation counts),
   and errors as single `ERROR` lines on stderr. Their buffers are allocated at init.
5. **Output:** nami's own binary format. One file per output, written in parallel with `pwrite`
   at offsets computed in advance. A self-describing header, then raw little-endian `f64`.
   Checkpoints use the same format and keep exact bits. A numpy-only Python reader lives in
   `tools/`. An XDMF sidecar is optional.
6. **Version metadata** in every output and checkpoint: `format_version`; `results_version`,
   which every `changes-results` PR increments; the nami commit with a dirty flag; the `rustc`
   version; a hash of `Cargo.lock`; the target and its CPU flags; the type names of the compiled
   combination; and the resolved configuration. A `std`-only build script gathers the build
   fields.
7. **Restart compares builds.** If a field that affects bits differs, the restart is refused.
   `--allow-different-build` overrides this and records it in the output.
8. **Guarantees:** with the same binary, configuration and toolchain, results are bitwise
   reproducible across platforms, across decompositions and across restarts. Bits are not
   guaranteed across `results_version` changes, and each such change is explained (ADR-0005).
9. **No Python bindings.**

## Enforcement

- Planned with M0: a test that a run interrupted by a checkpoint and restarted produces the same
  output hash as an uninterrupted run.
- Planned with M0: configuration tests for a missing key, an unknown key and a wrong type, each
  with its message.
- Planned: a CI check that a PR labelled `changes-results` increments `results_version`, and that
  a PR which changes output bits without the label fails the determinism hash (ADR-0005).
- Review: every new configuration key is documented where it is read.

## Consequences

- Any two outputs can be compared, and a mismatch in metadata explains a mismatch in bits before
  anyone has to debug.
- nami maintains a parser, a file format and a reader. They are small, but they are code without
  outside users.
- Existing tools read nami's output only through the optional sidecar, or not at all, until nami's
  own tools exist.
- Problem authors recompile to change methods (ADR-0025), but they never recompile to change
  parameters.

## Alternatives considered

- **Write HDF5 ourselves.** Rejected. The format is large, and nami's needs (parallel writes at
  fixed offsets, exact bits) are simple.
- **Athena++'s `.athdf`.** Rejected. It is HDF5.
- **A configuration format of our own design.** Rejected. A strict subset of TOML is familiar to
  humans and agents, and it costs nothing extra.
- **Allow restart with any build, silently.** Rejected. It would break the restart guarantee
  without anyone noticing.

## Evidence

- POSIX `pwrite` (available through `std::os::unix::fs::FileExt::write_at`).
- XDMF can describe raw binary data items, which ParaView and VisIt read.
- The maintainer's answers in the architecture discussion of 2026-11-26, including the request
  for version metadata to detect a changed toolchain.
