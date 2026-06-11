# Benchmark runner — external timing, Go / C / Python / Bolide JIT / Bolide AOT
param([int]$Runs = 5)

$Root = Split-Path -Parent $PSScriptRoot
$Bolide = "$Root\target\release\bolide.exe"

$benchmarks = @(
    @{Name="fib_rec(40)"; C="$Root\bench\fib_rec_c.exe";   Go="$Root\bench\fib_rec_go.exe";   BL="$Root\bench\fib_rec.bl";   BLAot="$Root\bench\fib_rec_bl.exe";   Py="$Root\bench\fib_rec.py"},
    @{Name="sieve(50M)";  C="$Root\bench\sieve_c.exe";     Go="$Root\bench\sieve_go.exe";     BL="$Root\bench\sieve.bl";     BLAot="$Root\bench\sieve_bl.exe";     Py="$Root\bench\sieve.py"}
)

function Invoke-Timed([string]$Label, [scriptblock]$Cmd, [int]$Iterations) {
    # 2 warm-up runs
    $null = & $Cmd 2>&1 | Out-Null
    Start-Sleep -Milliseconds 200
    $null = & $Cmd 2>&1 | Out-Null
    Start-Sleep -Milliseconds 200
    $times = foreach ($i in 1..$Iterations) {
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        $null = & $Cmd 2>&1 | Out-Null
        $sw.Elapsed.TotalSeconds
    }
    $avg = [Math]::Round(($times | Measure-Object -Average).Average, 3)
    $min = [Math]::Round(($times | Measure-Object -Minimum).Minimum, 3)
    $max = [Math]::Round(($times | Measure-Object -Maximum).Maximum, 3)
    return @{Label=$Label; Avg=$avg; Min=$min; Max=$max}
}

Write-Host @"

============================================================
  Language Benchmark: fib(40) + sieve 50,000,000
  Warm-up x2, then $Runs timed runs.  External stopwatch.
============================================================

"@

foreach ($bm in $benchmarks) {
    Write-Host ("--- {0} ---" -f $bm.Name)

    $go  = Invoke-Timed "Go (gc)"       { & $bm.Go }              $Runs
    $c   = Invoke-Timed "C (gcc -O3)"   { & $bm.C }               $Runs
    $py  = Invoke-Timed "Python 3"      { python $bm.Py }         $Runs
    $jit = Invoke-Timed "Bolide JIT"    { & $Bolide run $bm.BL }  $Runs
    $aot = Invoke-Timed "Bolide AOT"    { & $bm.BLAot }            $Runs

    $baseline = $c.Avg
    Write-Host ""
    Write-Host ("  {0,-14} {1,8:F3}s   {2,6:F3}s ~ {3,6:F3}s    vs C" -f "Language", "avg", "best", "worst")
    Write-Host "  " + ("-" * 48)
    foreach ($r in @($c, $go, $aot, $jit, $py) | Sort-Object Avg) {
        $mult = [Math]::Round($r.Avg / $baseline, 1)
        $m = if ($mult -le 1.05) { "1x" } else { "$($mult)x" }
        Write-Host ("  {0,-14} {1,8:F3}s   {2,6:F3}s ~ {3,6:F3}s   {4}" -f $r.Label, $r.Avg, $r.Min, $r.Max, $m)
    }
    Write-Host ""
}

Write-Host "Done."
