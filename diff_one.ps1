param([Parameter(Mandatory)][string]$File)
$tmp = Join-Path (Resolve-Path ".").Path ".diff_tmp"; New-Item -ItemType Directory -Force $tmp | Out-Null
$exe = Join-Path $tmp "d.exe"
function Norm([string]$s){ ($s -replace "`r","").Trim() }
$jitRaw = (& .\target\release\bolide.exe run $File 2>&1 | Out-String)
$lines = (Norm $jitRaw) -split "`n"
if ($lines[0] -match '^Running:') { $lines = $lines[1..($lines.Count-1)] }
if ($lines[-1] -match '^Result:') { $lines = $lines[0..($lines.Count-2)] }
$jit = ($lines -join "`n").Trim()
& .\target\release\bolide.exe compile $File -o $exe 2>&1 | Out-Null
if (-not (Test-Path $exe)) { Write-Host "AOT COMPILE FAILED"; & .\target\release\bolide.exe compile $File -o $exe 2>&1 | Out-String | Write-Host; exit }
$aot = Norm ((& $exe 2>&1 | Out-String))
Write-Host "===== JIT ====="; Write-Host $jit
Write-Host "===== AOT ====="; Write-Host $aot
Write-Host "===== MATCH: $($jit -eq $aot) ====="
Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
