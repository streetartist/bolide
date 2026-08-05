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

Reference machine: Windows, best of 3, self-reported `ms=`.  
C = `clang -O3 -march=native`. Bolide AOT via `--backend cranelift` (default) or `--backend llvm`.

| Benchmark | Args | C (ms) | Cranelift (ms) | LLVM (ms) | Clif/C | LLVM/C |
|---|---|---:|---:|---:|---:|---:|
| `fib` | 35 | 14 | 25 | **13** | 1.79× | **0.93×** |
| `sieve` | 5e6 | 71 | 78 | **68** | 1.10× | **0.96×** |
| `mandelbrot` | 800²×256 | 67 | 88 | **62** | 1.31× | **0.93×** |
| `nbody_perf` | 500×80 | 30 | 75 | **41** | 2.50× | **1.37×** |
| **geomean** | | | | | **~1.59×** | **~1.03×** |

Notes:

- **LLVM** is near C overall (~1.03× geomean): scalar loops often slightly faster than C; `list[i]`/`len` are **inlined** (same layout + bounds checks as Cranelift), not runtime calls.
- **Cranelift** remains the default full-language backend; list-heavy work is solid, scalar a bit behind LLVM.
- Checksums match C for all four (nbody float print may differ in last digits).
- Geometric mean is the headline number across releases; re-run after backend changes.

### Compare LLVM vs Cranelift yourself

```powershell
$env:BOLIDE_HOME = (Get-Location).Path
bolide compile bench/fib.bl -o tmp/fib_llvm.exe --backend llvm
bolide compile bench/fib.bl -o tmp/fib_clif.exe --backend cranelift
clang -O3 -march=native bench/fib.c -o tmp/fib_c.exe
./tmp/fib_llvm.exe 35 1
./tmp/fib_clif.exe 35 1
./tmp/fib_c.exe 35 1
```

## Adding a benchmark

1. Write `bench/<name>.bl` and `bench/<name>.c` with identical logic.
2. Both must accept their sizes as positional CLI args and print a line
   containing `ms=<n>` and `checksum=<...>`.
3. Add an entry to the `$benches` array in `run_all.ps1` with `Args` and
   `QuickArgs`.
4. Confirm the checksums match before trusting the timings.
