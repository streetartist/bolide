param(
    [string]$HostName = "127.0.0.1",
    [int]$Port = 8080
)

function Read-ExactBytes($Stream, [int]$Count) {
    $buf = New-Object byte[] $Count
    $offset = 0
    while ($offset -lt $Count) {
        $n = $Stream.Read($buf, $offset, $Count - $offset)
        if ($n -le 0) { throw "connection closed before body" }
        $offset += $n
    }
    return $buf
}

function Read-HttpResponse($Stream) {
    $bytes = New-Object System.Collections.Generic.List[byte]
    $one = New-Object byte[] 1
    while ($true) {
        $n = $Stream.Read($one, 0, 1)
        if ($n -le 0) { throw "connection closed before headers" }
        $bytes.Add($one[0])
        $count = $bytes.Count
        if ($count -ge 4) {
            if ($bytes[$count - 4] -eq 13 -and $bytes[$count - 3] -eq 10 -and $bytes[$count - 2] -eq 13 -and $bytes[$count - 1] -eq 10) {
                break
            }
        }
    }

    $headers = [System.Text.Encoding]::ASCII.GetString($bytes.ToArray())
    $length = 0
    foreach ($line in $headers.Split("`r`n")) {
        if ($line.ToLowerInvariant().StartsWith("content-length:")) {
            $length = [int]$line.Substring(15).Trim()
        }
    }
    if ($length -gt 0) {
        [void](Read-ExactBytes $Stream $length)
    }
    return $headers
}

$client = [System.Net.Sockets.TcpClient]::new($HostName, $Port)
$client.ReceiveTimeout = 5000
$client.SendTimeout = 5000
$stream = $client.GetStream()

$req1 = "GET / HTTP/1.1`r`nHost: $HostName`r`nConnection: keep-alive`r`n`r`n"
$bytes1 = [System.Text.Encoding]::ASCII.GetBytes($req1)
$stream.Write($bytes1, 0, $bytes1.Length)
$h1 = Read-HttpResponse $stream
Write-Host "FIRST-BEGIN"
Write-Host $h1
Write-Host "FIRST-END"

$req2 = "GET /about HTTP/1.1`r`nHost: $HostName`r`nConnection: close`r`n`r`n"
$bytes2 = [System.Text.Encoding]::ASCII.GetBytes($req2)
$stream.Write($bytes2, 0, $bytes2.Length)
$h2 = Read-HttpResponse $stream

$client.Close()

[pscustomobject]@{
    FirstStatus = ($h1.Split("`r`n")[0])
    FirstKeepAlive = $h1.ToLowerInvariant().Contains("connection: keep-alive")
    SecondStatus = ($h2.Split("`r`n")[0])
    SecondClose = $h2.ToLowerInvariant().Contains("connection: close")
} | Format-List
