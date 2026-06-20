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

`fib` is the outlier (~2.4x) because it is pure call overhead: the ratio there
measures prologue/epilogue and call-ABI cost with no loop body to amortize it.
The array/float benchmarks land around 1.4–1.6x, which is the cost of bounds
checks, ARC, and Cranelift's codegen versus LLVM `-O3`. The geometric mean is
the single headline number to watch across releases.

## Adding a benchmark

1. Write `bench/<name>.bl` and `bench/<name>.c` with identical logic.
2. Both must accept their sizes as positional CLI args and print a line
   containing `ms=<n>` and `checksum=<...>`.
3. Add an entry to the `$benches` array in `run_all.ps1` with `Args` and
   `QuickArgs`.
4. Confirm the checksums match before trusting the timings.
