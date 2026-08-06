param([Parameter(Mandatory)][string]$File)
$tmp = Join-Path (Resolve-Path ".").Path ".llvm_diff_tmp"; New-Item -ItemType Directory -Force $tmp | Out-Null
$exe = Join-Path $tmp "d.exe"
function Norm([string]$s){ ($s -replace "`r","").Trim() }
# JIT golden
$jitRaw = (& .\target\release\bolide.exe run $File 2>&1 | Out-String)
$lines = (Norm $jitRaw) -split "`n"
if ($lines[0] -match '^Running:') { $lines = $lines[1..($lines.Count-1)] }
if ($lines[-1] -match '^Result:') { $lines = $lines[0..($lines.Count-2)] }
$jit = ($lines -join "`n").Trim()
# LLVM compile
$comp = & .\target\release\bolide.exe compile $File -o $exe --backend llvm 2>&1 | Out-String
if (-not (Test-Path $exe)) { Write-Host "LLVM COMPILE FAILED"; Write-Host $comp; Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue; exit }
$llvm = Norm ((& $exe 2>&1 | Out-String))
Write-Host "===== JIT ====="; Write-Host $jit
Write-Host "===== LLVM ====="; Write-Host $llvm
Write-Host "===== MATCH: $($jit -eq $llvm) ====="
Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
