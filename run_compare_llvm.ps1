# Compare Cranelift JIT output (golden reference) vs LLVM backend output for all .bl files.
# Usage: pwsh run_compare_llvm.ps1 [dir] [throttle] [timeoutSec]
param(
    [string]$Dir = "tests",
    [int]$Throttle = 8,
    [int]$TimeoutSec = 20
)

$ErrorActionPreference = "Continue"
$Bolide = (Resolve-Path ".\target\release\bolide.exe").Path
$Tmp = Join-Path (Resolve-Path ".").Path (".cmp_llvm_" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $Tmp -Force | Out-Null

$files = Get-ChildItem -Path $Dir -Filter *.bl

$results = $files | ForEach-Object -Parallel {
    $Bolide = $using:Bolide
    $Tmp = $using:Tmp
    $TimeoutSec = $using:TimeoutSec
    $f = $_.FullName
    $name = $_.Name

    # Run an exe with timeout, capturing stdout+stderr. Returns @{ Out; TimedOut }
    function Invoke-WithTimeout($exe, $argList, $secs, $tag) {
        $outF = Join-Path $Tmp ($tag + ".out")
        $errF = Join-Path $Tmp ($tag + ".err")
        $p = Start-Process -FilePath $exe -ArgumentList $argList -NoNewWindow -PassThru `
            -RedirectStandardOutput $outF -RedirectStandardError $errF
        $timedOut = $false
        if (-not $p.WaitForExit($secs * 1000)) {
            $timedOut = $true
            try { $p.Kill($true) } catch {}
            try { $p.WaitForExit(2000) | Out-Null } catch {}
        }
        $o = ""
        if (Test-Path $outF) { $o += (Get-Content $outF -Raw -ErrorAction SilentlyContinue) }
        if (Test-Path $errF) { $o += (Get-Content $errF -Raw -ErrorAction SilentlyContinue) }
        Remove-Item $outF,$errF -ErrorAction SilentlyContinue
        return @{ Out = $o; TimedOut = $timedOut }
    }

    function Strip-JitWrapper([string]$raw) {
        $raw = ($raw -replace "`r", "").Trim()
        $lines = $raw -split "`n"
        if ($lines.Count -ge 1 -and $lines[0] -match '^Running:') { $lines = $lines[1..($lines.Count-1)] }
        if ($lines.Count -ge 1 -and $lines[-1] -match '^Result:') {
            if ($lines.Count -eq 1) { $lines = @() } else { $lines = $lines[0..($lines.Count-2)] }
        }
        return ($lines -join "`n").Trim()
    }
    function Normalize([string]$s) { return ($s -replace "`r", "").Trim() }

    # Golden reference: Cranelift JIT run
    $jit = Invoke-WithTimeout $Bolide @("run", $f) $TimeoutSec ($name + ".jit")
    $jitOut = Strip-JitWrapper $jit.Out

    # LLVM AOT compile
    $exe = Join-Path $Tmp ($name + ".exe")
    $comp = Invoke-WithTimeout $Bolide @("compile", $f, "-o", $exe, "--backend", "llvm") $TimeoutSec ($name + ".comp")

    $status = ""; $detail = ""
    if ($comp.TimedOut) {
        $status = "CTIMEOUT"
    } elseif (-not (Test-Path $exe)) {
        $status = "COMPILE"
        $detail = ($comp.Out -split "`r?`n" | Where-Object { $_ -match 'error|Error|error:' } | Select-Object -First 1)
    } else {
        $run = Invoke-WithTimeout $exe @() $TimeoutSec ($name + ".run")
        if ($run.TimedOut) {
            $status = "RTIMEOUT"
        } else {
            $llvmOut = Normalize $run.Out
            if ($jitOut -eq $llvmOut) { $status = "PASS" }
            else { $status = "DIFF" }
        }
    }
    [PSCustomObject]@{ Name = $name; Status = $status; Detail = $detail }
} -ThrottleLimit $Throttle

$pass = ($results | Where-Object { $_.Status -eq "PASS" }).Count
$fail = ($results | Where-Object { $_.Status -ne "PASS" }).Count

Write-Host "==================================="
Write-Host "$Dir (LLVM vs JIT): PASS=$pass FAIL=$fail / $($files.Count)"
Write-Host "-----------------------------------"
$results | Where-Object { $_.Status -ne "PASS" } | Sort-Object Status,Name | ForEach-Object {
    Write-Host ("[{0,-8}] {1}  {2}" -f $_.Status, $_.Name, $_.Detail)
}

Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
