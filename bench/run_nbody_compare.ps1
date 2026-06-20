param(
    [int]$Bodies = 900,
    [int]$Steps = 120,
    [string]$Bolide = "$PSScriptRoot\..\target\release\bolide.exe"
)

$ErrorActionPreference = "Stop"

$root = Resolve-Path "$PSScriptRoot\.."
$outDir = Join-Path $root "tmp"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

$bolideSource = Join-Path $PSScriptRoot "nbody_perf.bl"
$bolideExe = Join-Path $outDir "nbody_perf_bolide.exe"
$cSource = Join-Path $PSScriptRoot "nbody_perf.c"
$cExe = Join-Path $outDir "nbody_perf_c.exe"

Write-Host "Building Bolide AOT..."
& $Bolide compile $bolideSource -o $bolideExe
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$compiler = $null
$compilerArgs = @()
if (Get-Command clang -ErrorAction SilentlyContinue) {
    $compiler = "clang"
    $compilerArgs = @("-O3", "-march=native", $cSource, "-o", $cExe)
} elseif (Get-Command gcc -ErrorAction SilentlyContinue) {
    $compiler = "gcc"
    $compilerArgs = @("-O3", "-march=native", $cSource, "-lm", "-o", $cExe)
} elseif (Get-Command cl -ErrorAction SilentlyContinue) {
    $compiler = "cl"
    $compilerArgs = @("/O2", "/Fe:$cExe", $cSource)
} else {
    throw "No C compiler found. Install clang, gcc, or MSVC cl."
}

Write-Host "Building C with $compiler..."
& $compiler @compilerArgs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host ""
Write-Host "Running benchmark: bodies=$Bodies steps=$Steps"
Write-Host "Bolide:"
& $bolideExe $Bodies $Steps
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "C:"
& $cExe $Bodies $Steps
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
