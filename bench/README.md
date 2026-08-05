# Bolide Benchmarks

Microbenchmarks comparing Bolide AOT-compiled native executables against
equivalent hand-written C compiled with `-O3 -march=native`. Each benchmark
exists as a `.bl` and a structurally-identical `.c` file so the comparison is
apples-to-apples: same algorithm, same data sizes, same checksum.

## Running

```powershell
# Build + run the whole suite, best of 3, at production sizes
pwsh -File bench/run_all.ps1

# Faster pass with smaller inputs
pwsh -File bench/run_all.ps1 -Quick

# More samples / a subset
pwsh -File bench/run_all.ps1 -Runs 5 -Only fib,sieve
```

The runner builds each benchmark (Bolide AOT + native C), does one warmup run,
then takes the **minimum** self-reported wall-clock time over `-Runs` samples
and prints a table with the Bolide/C slowdown ratio and the geometric mean.

A C compiler is required (`clang` preferred, then `gcc`, then MSVC `cl`).

## Benchmarks

Each one isolates a different cost so a regression points at a specific part of
the codegen, not a blended average.

| Benchmark | Stresses | Notes |
|---|---|---|
| `fib` | function-call overhead, integer ALU, branches | naive recursion, near-zero memory traffic |
| `sieve` | integer array indexing, strided memory writes, bounds checks | `list<int>` rebuilt each rep; C mirrors with a 64-bit array |
| `mandelbrot` | tight scalar float loop, data-dependent branch | single-threaded, no memory traffic |
| `nbody_perf` | float math + list index/mutate + function calls | O(n^2 * steps) gravitational sim |

Checksums are printed by every program and **must match** between Bolide and C
— they do today, which is what makes the timing comparison valid.

## Interpreting results

Typical local ratios after the performance passes (best of 3, Windows,
Cranelift `opt_level=speed` vs clang `-O3 -march=native`):

| Benchmark | Bolide / C | Notes |
|---|---|---|
| `fib` | ~1.8x | pure call overhead; skip exception checks when no `throw`/`try`/`?` |
| `sieve` | ~1.1–1.2x | `list.resize` bulk init; `list[i]` keeps bounds checks |
| `mandelbrot` | ~1.4x | float loop codegen; auto-inlines small leaf helpers |
| `nbody_perf` | ~1.2x | math intrinsics + list access + auto-inline |

`fib` remains the outlier because it is pure call overhead: the ratio measures
prologue/epilogue and call-ABI cost with no loop body to amortize it. The
geometric mean is the single headline number to watch across releases.

## Adding a benchmark

1. Write `bench/<name>.bl` and `bench/<name>.c` with identical logic.
2. Both must accept their sizes as positional CLI args and print a line
   containing `ms=<n>` and `checksum=<...>`.
3. Add an entry to the `$benches` array in `run_all.ps1` with `Args` and
   `QuickArgs`.
4. Confirm the checksums match before trusting the timings.
