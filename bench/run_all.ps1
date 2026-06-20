# Unified Bolide-vs-C benchmark runner.
#
# Builds every benchmark in this directory (Bolide AOT + native C), runs each
# one `Runs` times after a warmup, takes the best (minimum) wall-clock time
# reported by the program itself, and prints a comparison table with the
# Bolide/C slowdown ratio.
#
# Usage:
#   pwsh -File bench/run_all.ps1                 # default sizes
#   pwsh -File bench/run_all.ps1 -Runs 5         # more samples
#   pwsh -File bench/run_all.ps1 -Only fib,sieve # subset
#   pwsh -File bench/run_all.ps1 -Quick          # smaller sizes for a fast pass

param(
    [int]$Runs = 3,
    [string[]]$Only = @(),
    [switch]$Quick,
    [string]$Bolide = "$PSScriptRoot\..\target\release\bolide.exe"
)

$ErrorActionPreference = "Stop"

$root = Resolve-Path "$PSScriptRoot\.."
$outDir = Join-Path $root "tmp"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

# Benchmark definitions: name -> argument list passed to both executables.
# Each entry: @{ Name; Args; QuickArgs }
$benches = @(
    @{ Name = "fib";        Args = @(35, 1);             QuickArgs = @(30, 1) },
    @{ Name = "sieve";      Args = @(20000000, 1);       QuickArgs = @(2000000, 1) },
    @{ Name = "mandelbrot"; Args = @(1200, 1200, 256);   QuickArgs = @(500, 500, 256) },
    @{ Name = "nbody_perf"; Args = @(900, 120);          QuickArgs = @(400, 60) }
)

if ($Only.Count -gt 0) {
    $benches = $benches | Where-Object { $Only -contains $_.Name }
    if ($benches.Count -eq 0) { throw "No benchmarks matched -Only: $($Only -join ', ')" }
}

# Locate a C compiler once.
$cc = $null
$ccFlags = $null
if (Get-Command clang -ErrorAction SilentlyContinue) {
    $cc = "clang"; $ccFlags = { param($src, $exe) @("-O3", "-march=native", $src, "-o", $exe) }
} elseif (Get-Command gcc -ErrorAction SilentlyContinue) {
    $cc = "gcc"; $ccFlags = { param($src, $exe) @("-O3", "-march=native", $src, "-lm", "-o", $exe) }
} elseif (Get-Command cl -ErrorAction SilentlyContinue) {
    $cc = "cl"; $ccFlags = { param($src, $exe) @("/O2", "/Fe:$exe", $src) }
} else {
    throw "No C compiler found. Install clang, gcc, or MSVC cl."
}

function Get-Ms($line) {
    if ($line -match "ms=(\d+)") { return [int]$Matches[1] }
    return -1
}

function Invoke-Best($exe, $argList, $runs) {
    # One warmup run (discarded), then take the minimum reported ms.
    & $exe @argList | Out-Null
    $best = [int]::MaxValue
    $last = ""
    for ($i = 0; $i -lt $runs; $i++) {
        $line = (& $exe @argList | Where-Object { $_ -match "ms=" } | Select-Object -Last 1)
        $ms = Get-Ms $line
        if ($ms -ge 0 -and $ms -lt $best) { $best = $ms; $last = $line }
    }
    return @{ Ms = $best; Line = $last }
}

$results = @()

foreach ($b in $benches) {
    $name = $b.Name
    $argList = if ($Quick) { $b.QuickArgs } else { $b.Args }

    $blSrc = Join-Path $PSScriptRoot "$name.bl"
    $cSrc = Join-Path $PSScriptRoot "$name.c"
    $blExe = Join-Path $outDir "${name}_bolide.exe"
    $cExe = Join-Path $outDir "${name}_c.exe"

    if (-not (Test-Path $blSrc)) { Write-Host "skip $name (no .bl)"; continue }
    if (-not (Test-Path $cSrc)) { Write-Host "skip $name (no .c)"; continue }

    Write-Host "Building $name (Bolide AOT)..."
    & $Bolide compile $blSrc -o $blExe | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "Bolide compile failed for $name" }

    Write-Host "Building $name (C / $cc)..."
    & $cc @(& $ccFlags $cSrc $cExe)
    if ($LASTEXITCODE -ne 0) { throw "C compile failed for $name" }

    Write-Host "Running $name (args: $($argList -join ' '), best of $Runs)..."
    $bl = Invoke-Best $blExe $argList $Runs
    $c = Invoke-Best $cExe $argList $Runs

    $ratio = if ($c.Ms -gt 0) { [math]::Round($bl.Ms / $c.Ms, 2) } else { 0 }
    $results += [pscustomobject]@{
        Benchmark = $name
        Args      = ($argList -join " ")
        BolideMs  = $bl.Ms
        CMs       = $c.Ms
        Ratio     = $ratio
    }
    Write-Host ""
}

Write-Host "=================== RESULTS (best of $Runs) ==================="
$results | Format-Table -AutoSize Benchmark, Args, BolideMs, CMs, @{ Name = "Bolide/C"; Expression = { "{0:N2}x" -f $_.Ratio } }

if ($results.Count -gt 0) {
    $geo = ($results | ForEach-Object { [math]::Log($_.Ratio) } | Measure-Object -Average).Average
    $geomean = [math]::Exp($geo)
    Write-Host ("Geometric-mean slowdown vs C: {0:N2}x" -f $geomean)
}
