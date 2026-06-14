param(
    [ValidateSet("CompareHello", "BolideSync", "BolideAsync", "GoHello", "Blog", "GoBlog", "BolideFastBlog", "CompareBlog", "CompareFastBlog", "Url")]
    [string]$Target = "CompareHello",
    [int]$Requests = 100000,
    [int]$Concurrency = 128,
    [int]$Warmup = 5000,
    [string]$Paths = "/,/hello/world",
    [string]$Url = "",
    [int]$CooldownSeconds = 0,
    [switch]$NoBuild,
    [switch]$NoKeepAlive
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$Bench = $PSScriptRoot
$GoWork = Join-Path $Bench ".go-work"
New-Item -ItemType Directory -Force -Path $GoWork | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $GoWork "cache") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $GoWork "telemetry") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $GoWork "appdata") | Out-Null
$env:APPDATA = Join-Path $GoWork "appdata"
$env:GOTELEMETRY = "off"
$env:GOTELEMETRYDIR = Join-Path $GoWork "telemetry"
$env:GOCACHE = Join-Path $GoWork "cache"

$LoadExe = Join-Path $Bench "http_load.exe"
$GoServerExe = Join-Path $Bench "http_go_hello.exe"
$GoBlogExe = Join-Path $Bench "http_go_blog.exe"
$BolideSyncExe = Join-Path $Bench "http_bolide_hello_sync.exe"
$BolideAsyncExe = Join-Path $Bench "http_bolide_hello_async.exe"
$BolideFastBlogExe = Join-Path $Bench "http_bolide_blog_fast.exe"
$Bolide = Join-Path $Root "bolide.exe"

function Invoke-Cmd([string]$File, [string[]]$CommandArgs) {
    & $File @CommandArgs
    if ($LASTEXITCODE -ne 0) {
        throw "$File failed with exit code $LASTEXITCODE"
    }
}

function Build-Tools {
    if ($NoBuild) { return }
    Invoke-Cmd "go" @("build", "-o", $LoadExe, (Join-Path $Bench "http_load.go"))
    Invoke-Cmd "go" @("build", "-o", $GoServerExe, (Join-Path $Bench "http_go_hello.go"))
    Invoke-Cmd "go" @("build", "-o", $GoBlogExe, (Join-Path $Bench "http_go_blog.go"))
    Invoke-Cmd $Bolide @("compile", (Join-Path $Bench "http_bolide_hello_sync.bl"), "-o", $BolideSyncExe)
    Invoke-Cmd $Bolide @("compile", (Join-Path $Bench "http_bolide_hello_async.bl"), "-o", $BolideAsyncExe)
    Invoke-Cmd $Bolide @("compile", (Join-Path $Bench "http_bolide_blog_fast.bl"), "-o", $BolideFastBlogExe)
}

function Wait-Http([string]$BaseUrl) {
    $client = [System.Net.Http.HttpClient]::new()
    try {
        $deadline = [DateTime]::UtcNow.AddSeconds(10)
        while ([DateTime]::UtcNow -lt $deadline) {
            try {
                $resp = $client.GetAsync($BaseUrl).Result
                $resp.Dispose()
                return
            } catch {
                Start-Sleep -Milliseconds 100
            }
        }
        throw "server did not become ready: $BaseUrl"
    } finally {
        $client.Dispose()
    }
}

function Start-Server([string]$Exe, [string[]]$CommandArgs, [string]$WorkingDirectory, [string]$BaseUrl) {
    $proc = Start-Process -FilePath $Exe -ArgumentList $CommandArgs -WorkingDirectory $WorkingDirectory -PassThru -WindowStyle Hidden
    try {
        Wait-Http $BaseUrl
        return $proc
    } catch {
        if (-not $proc.HasExited) {
            Stop-Process -Id $proc.Id -Force
        }
        throw
    }
}

function Stop-Server($Proc) {
    if ($null -ne $Proc -and -not $Proc.HasExited) {
        Stop-Process -Id $Proc.Id -Force
    }
}

function Invoke-Load([string]$Label, [string]$BaseUrl, $ServerProc) {
    $loadArgs = @(
        "-url", $BaseUrl,
        "-paths", $Paths,
        "-n", "$Requests",
        "-c", "$Concurrency",
        "-warmup", "$Warmup",
        "-label", $Label
    )
    if ($NoKeepAlive) {
        $loadArgs += "-no-keepalive"
    }

    Write-Host ""
    Write-Host "===== $Label ====="
    if ($null -ne $ServerProc) {
        $ServerProc.Refresh()
        $startPrivate = $ServerProc.PrivateMemorySize64
        $startWs = $ServerProc.WorkingSet64
    }

    Invoke-Cmd $LoadExe $loadArgs

    if ($null -ne $ServerProc -and -not $ServerProc.HasExited) {
        $ServerProc.Refresh()
        $endPrivate = $ServerProc.PrivateMemorySize64
        $endWs = $ServerProc.WorkingSet64
        Write-Host ("server_private: {0:F2} MB -> {1:F2} MB" -f ($startPrivate / 1MB), ($endPrivate / 1MB))
        Write-Host ("server_ws:      {0:F2} MB -> {1:F2} MB" -f ($startWs / 1MB), ($endWs / 1MB))
        if ($CooldownSeconds -gt 0) {
            Start-Sleep -Seconds $CooldownSeconds
            if (-not $ServerProc.HasExited) {
                $ServerProc.Refresh()
                Write-Host ("server_private_after_{0}s: {1:F2} MB" -f $CooldownSeconds, ($ServerProc.PrivateMemorySize64 / 1MB))
                Write-Host ("server_ws_after_{0}s:      {1:F2} MB" -f $CooldownSeconds, ($ServerProc.WorkingSet64 / 1MB))
            }
        }
    }
}

function Run-GoHello {
    $proc = $null
    try {
        $proc = Start-Server $GoServerExe @("-addr", "127.0.0.1:18080") $Root "http://127.0.0.1:18080/"
        Invoke-Load "go-hello" "http://127.0.0.1:18080" $proc
    } finally {
        Stop-Server $proc
    }
}

function Run-BolideSync {
    $proc = $null
    try {
        $proc = Start-Server $BolideSyncExe @() $Root "http://127.0.0.1:18081/"
        Invoke-Load "bolide-sync" "http://127.0.0.1:18081" $proc
    } finally {
        Stop-Server $proc
    }
}

function Run-BolideAsync {
    $proc = $null
    try {
        $proc = Start-Server $BolideAsyncExe @() $Root "http://127.0.0.1:18082/"
        Invoke-Load "bolide-async" "http://127.0.0.1:18082" $proc
    } finally {
        Stop-Server $proc
    }
}

function Use-BlogPaths {
    if ($script:Paths -eq "/,/hello/world") {
        $script:Paths = "/,/about,/posts/1,/posts/2,/posts/3,/admin"
    }
}

function Run-Blog {
    $proc = $null
    $blogExe = Join-Path $Root "examples\blog\main.exe"
    $blogDir = Join-Path $Root "examples\blog"
    $oldPaths = $script:Paths
    Use-BlogPaths
    try {
        $proc = Start-Server $blogExe @() $blogDir "http://127.0.0.1:8080/"
        Invoke-Load "bolide-blog" "http://127.0.0.1:8080" $proc
    } finally {
        $script:Paths = $oldPaths
        Stop-Server $proc
    }
}

function Run-GoBlog {
    $proc = $null
    $oldPaths = $script:Paths
    Use-BlogPaths
    try {
        $proc = Start-Server $GoBlogExe @("-addr", "127.0.0.1:18083") $Root "http://127.0.0.1:18083/"
        Invoke-Load "go-blog" "http://127.0.0.1:18083" $proc
    } finally {
        $script:Paths = $oldPaths
        Stop-Server $proc
    }
}

function Run-BolideFastBlog {
    $proc = $null
    $oldPaths = $script:Paths
    Use-BlogPaths
    try {
        $proc = Start-Server $BolideFastBlogExe @() $Root "http://127.0.0.1:18084/"
        Invoke-Load "bolide-fast-blog" "http://127.0.0.1:18084" $proc
    } finally {
        $script:Paths = $oldPaths
        Stop-Server $proc
    }
}

Build-Tools

switch ($Target) {
    "CompareHello" {
        Run-GoHello
        Run-BolideSync
        Run-BolideAsync
    }
    "GoHello" { Run-GoHello }
    "BolideSync" { Run-BolideSync }
    "BolideAsync" { Run-BolideAsync }
    "Blog" { Run-Blog }
    "GoBlog" { Run-GoBlog }
    "BolideFastBlog" { Run-BolideFastBlog }
    "CompareBlog" {
        Run-GoBlog
        Run-Blog
    }
    "CompareFastBlog" {
        Run-GoBlog
        Run-BolideFastBlog
    }
    "Url" {
        if ($Url -eq "") {
            throw "-Url is required when -Target Url"
        }
        Invoke-Load "url" $Url $null
    }
}
