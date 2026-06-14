param(
    [string]$BaseUrl = "http://127.0.0.1:8080",
    [int]$Count = 100,
    [int[]]$PostIds = @(),
    [int]$DelayMs = 0,
    [int]$ProcessId = 0,
    [string]$ProcessName = ""
)

$ErrorActionPreference = "Stop"
$BaseUrl = $BaseUrl.TrimEnd("/")

function Get-TargetProcess {
    if ($ProcessId -gt 0) {
        return Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
    }

    if ($ProcessName -ne "") {
        return Get-Process -Name $ProcessName -ErrorAction SilentlyContinue |
            Sort-Object StartTime -Descending |
            Select-Object -First 1
    }

    return $null
}

function Write-MemorySample {
    param([string]$Label)

    $proc = Get-TargetProcess
    if ($null -eq $proc) {
        return
    }

    $workingSetMb = [math]::Round($proc.WorkingSet64 / 1MB, 2)
    $privateMb = [math]::Round($proc.PrivateMemorySize64 / 1MB, 2)
    Write-Host ("[{0}] pid={1} ws={2}MB private={3}MB" -f $Label, $proc.Id, $workingSetMb, $privateMb)
}

function Request-Text {
    param(
        [System.Net.Http.HttpClient]$Client,
        [string]$Path
    )

    $uri = "$BaseUrl$Path"
    $response = $Client.GetAsync($uri).GetAwaiter().GetResult()
    $body = $response.Content.ReadAsStringAsync().GetAwaiter().GetResult()
    if (-not $response.IsSuccessStatusCode) {
        throw "GET $uri failed with HTTP $([int]$response.StatusCode)"
    }
    return $body
}

Add-Type -AssemblyName System.Net.Http

$handler = [System.Net.Http.HttpClientHandler]::new()
$client = [System.Net.Http.HttpClient]::new($handler)
$client.Timeout = [TimeSpan]::FromSeconds(10)
$client.DefaultRequestHeaders.CacheControl =
    [System.Net.Http.Headers.CacheControlHeaderValue]::Parse("no-cache")
$client.DefaultRequestHeaders.Pragma.ParseAdd("no-cache")

try {
    if ($PostIds.Count -eq 0) {
        Write-Host "Discovering post links from / and /admin ..."
        $pages = @(
            (Request-Text -Client $client -Path "/"),
            (Request-Text -Client $client -Path "/admin")
        )

        $found = New-Object "System.Collections.Generic.HashSet[int]"
        foreach ($page in $pages) {
            foreach ($match in [regex]::Matches($page, 'href="/posts/([0-9]+)"')) {
                [void]$found.Add([int]$match.Groups[1].Value)
            }
        }

        $PostIds = $found | Sort-Object
    }

    if ($PostIds.Count -eq 0) {
        throw "No post ids found. Pass ids explicitly, for example: -PostIds 1,2,3"
    }

    Write-Host ("BaseUrl={0}" -f $BaseUrl)
    Write-Host ("PostIds={0}" -f ($PostIds -join ","))
    Write-Host ("CountPerPost={0}" -f $Count)
    Write-MemorySample "start"

    $total = 0
    foreach ($id in $PostIds) {
        $path = "/posts/$id"
        Write-Host ("Requesting {0} x {1}" -f $path, $Count)

        for ($i = 1; $i -le $Count; $i++) {
            $body = Request-Text -Client $client -Path $path
            $total++

            if ($body.Length -eq 0) {
                throw "GET $BaseUrl$path returned an empty body"
            }

            if (($i % 10) -eq 0 -or $i -eq $Count) {
                Write-Host ("  {0}/{1} len={2}" -f $i, $Count, $body.Length)
                Write-MemorySample ("post {0} #{1}" -f $id, $i)
            }

            if ($DelayMs -gt 0) {
                Start-Sleep -Milliseconds $DelayMs
            }
        }
    }

    Write-Host ("Done. Requests={0}" -f $total)
    Write-MemorySample "end"
}
finally {
    $client.Dispose()
    $handler.Dispose()
}
