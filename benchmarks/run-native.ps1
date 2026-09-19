param([int]$Seconds = 10, [int]$Repeats = 3, [int]$Concurrency = 32,
      [string]$BinaryDirectory = 'target/v2-check/release',
      [string]$ConfigurationFile = 'benchmarks/native.toml',
      [ValidateSet('all', 'small.txt', 'medium.txt', 'large.bin')][string]$File = 'all')
$ErrorActionPreference = 'Stop'
if ($Seconds -lt 1 -or $Repeats -lt 1 -or $Concurrency -lt 1) { throw 'Use positive parameters' }
$projectRoot = Split-Path $PSScriptRoot -Parent
$binary = Join-Path $projectRoot "$BinaryDirectory/flashlin.exe"
$runner = Join-Path $projectRoot "$BinaryDirectory/examples/benchmark.exe"
if (!(Test-Path -LiteralPath $binary) -or !(Test-Path -LiteralPath $runner)) { throw "Build the release server and benchmark example in $BinaryDirectory first" }
$resultPath = Join-Path $PSScriptRoot ('results/native-' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
New-Item -ItemType Directory -Path $resultPath -Force | Out-Null
& (Join-Path $PSScriptRoot 'prepare.ps1')
$probe = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 18082)
try { $probe.Start() } finally { $probe.Stop() }
$server = Start-Process -FilePath $binary -ArgumentList ('"' + $ConfigurationFile + '"') -WorkingDirectory $projectRoot -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $resultPath 'server.log') -RedirectStandardError (Join-Path $resultPath 'server-error.log')
$measurements = @()
try {
    Start-Sleep -Milliseconds 500
    if ($server.HasExited) { throw 'Benchmark server exited at startup; see server-error.log' }
    # Hardware inventory may be restricted; HTTP measurements do not need it.
    $inventoryError = $null
    try {
        $cpu = Get-CimInstance Win32_Processor | Select-Object -First 1 Name, NumberOfCores, NumberOfLogicalProcessors
        $machine = Get-CimInstance Win32_ComputerSystem | Select-Object TotalPhysicalMemory
    } catch {
        $cpu = @{ NumberOfLogicalProcessors = [Environment]::ProcessorCount }
        $machine = $null
        $inventoryError = $_.Exception.Message
    }
    foreach ($iteration in 1..$Repeats) {
        foreach ($sample in @(@('small.txt', 1024), @('medium.txt', 65536), @('large.bin', 4194304))) {
            if ($File -ne 'all' -and $sample[0] -ne $File) { continue }
            $name = "$($sample[0])-$iteration"
            $out = Join-Path $resultPath "$name.json"
            $server.Refresh()
            $cpuBefore = $server.TotalProcessorTime.TotalSeconds
            $started = [Diagnostics.Stopwatch]::StartNew()
            $peakMemory = 0L
            $load = Start-Process -FilePath $runner -ArgumentList @("http://127.0.0.1:18082/$($sample[0])", $Seconds, $Concurrency, $sample[1]) -WorkingDirectory $projectRoot -WindowStyle Hidden -PassThru -RedirectStandardOutput $out -RedirectStandardError (Join-Path $resultPath "$name-error.log")
            try {
                while (!$load.HasExited) {
                    $server.Refresh()
                    if ($server.HasExited) { throw 'Server exited during measurement' }
                    $peakMemory = [Math]::Max($peakMemory, $server.WorkingSet64)
                    Start-Sleep -Milliseconds 250
                }
                $load.WaitForExit()
                if ($load.ExitCode -ne 0) { throw "Benchmark $name failed; inspect raw results" }
            } finally { if (!$load.HasExited) { Stop-Process -Id $load.Id } }
            $server.Refresh()
            $result = Get-Content -Raw -LiteralPath $out | ConvertFrom-Json
            $result | Add-Member -NotePropertyName iteration -NotePropertyValue $iteration
            $result | Add-Member -NotePropertyName server_peak_sampled_working_set_bytes -NotePropertyValue $peakMemory
            $result | Add-Member -NotePropertyName server_cpu_percent_one_core_including_warmup -NotePropertyValue (100 * ($server.TotalProcessorTime.TotalSeconds - $cpuBefore) / $started.Elapsed.TotalSeconds)
            $measurements += $result
            Write-Output "$name : $([Math]::Round($result.successful_requests_per_second, 1)) requests/sec; $($result.errors) errors"
        }
    }
    [ordered]@{
        date_utc = [DateTime]::UtcNow.ToString('o')
        platform = [Environment]::OSVersion.VersionString
        cpu = $cpu
        memory = $machine
        hardware_inventory_error = $inventoryError
        binary_sha256 = (Get-FileHash -LiteralPath $binary).Hash
        configuration_file = $ConfigurationFile
        config_sha256 = (Get-FileHash -LiteralPath (Join-Path $projectRoot $ConfigurationFile)).Hash
        scope = 'Windows loopback; server and client share this machine; warm OS cache; not a production or cross-server result'
        results = $measurements
    } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $resultPath 'report.json') -Encoding utf8
    Write-Output "Report: $resultPath/report.json"
} finally {
    if (!$server.HasExited) { Stop-Process -Id $server.Id }
}
