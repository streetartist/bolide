# Peak memory — simplest possible Process API, no fancy flags
$Root = Split-Path -Parent $PSScriptRoot

function PeakMB([string]$Label, [string]$Exe, [string[]]$Args) {
    Write-Host -NoNewline ("  {0,-14}  " -f $Label)

    $proc = New-Object System.Diagnostics.Process
    $proc.StartInfo.FileName = $Exe
    if ($Args) { $proc.StartInfo.Arguments = $Args -join " " }
    $proc.StartInfo.UseShellExecute = $false
    $proc.StartInfo.RedirectStandardOutput = $true

    $proc.Start() | Out-Null

    $peak = 0
    while (-not $proc.HasExited) {
        # 必须 Refresh，否则 WorkingSet64 一直返回首次读取的缓存值
        $proc.Refresh()
        try {
            $ws = $proc.WorkingSet64
            if ($ws -gt $peak) { $peak = $ws }
        } catch {}
        Start-Sleep -Milliseconds 5
    }
    # Process just exited — get final peak before disposing
    try { $ws = $proc.WorkingSet64; if ($ws -gt $peak) { $peak = $ws } } catch {}
    try { $pws = $proc.PeakWorkingSet64; if ($pws -gt $peak) { $peak = $pws } } catch {}
    $proc.Dispose()

    Write-Host ("{0,6:F1} MB" -f ($peak / 1048576))
}

Write-Host "Peak memory (WS), single run`n"

Write-Host "===== fib(40) ====="
PeakMB "C (gcc -O3)"   "$Root\bench\fib_rec_c.exe"
PeakMB "Go (gc)"       "$Root\bench\fib_rec_go.exe"
PeakMB "Swift 6.3 (-O)" "$Root\bench\fib_rec_swift.exe"
PeakMB "Bolide AOT"    "$Root\bench\fib_rec_bl.exe"

Write-Host "`n===== sieve(50M) ====="
PeakMB "C (gcc -O3)"   "$Root\bench\sieve_c.exe"
PeakMB "Go (gc)"       "$Root\bench\sieve_go.exe"
PeakMB "Swift 6.3 (-O)" "$Root\bench\sieve_swift.exe"
PeakMB "Bolide AOT"    "$Root\bench\sieve_bl.exe"

Write-Host "`nDone."
