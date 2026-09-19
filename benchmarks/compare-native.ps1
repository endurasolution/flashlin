param(
    [Parameter(Mandatory=$true)][string]$BeforeDirectory,
    [Parameter(Mandatory=$true)][string]$AfterDirectory,
    [int]$Seconds = 10,
    [int]$Repeats = 3,
    [int]$Concurrency = 32,
    [string]$ConfigurationFile = 'benchmarks/native.toml',
    [ValidateSet('all', 'small.txt', 'medium.txt', 'large.bin')][string]$File = 'all'
)
$ErrorActionPreference = 'Stop'
if ($Seconds -lt 1 -or $Repeats -lt 1 -or $Concurrency -lt 1) { throw 'Use positive parameters' }
$projectRoot = Split-Path $PSScriptRoot -Parent
$beforeRunner = Join-Path $projectRoot "$BeforeDirectory/examples/benchmark.exe"
$afterRunner = Join-Path $projectRoot "$AfterDirectory/examples/benchmark.exe"
if ((Get-FileHash -LiteralPath $beforeRunner).Hash -ne (Get-FileHash -LiteralPath $afterRunner).Hash) {
    throw 'Use the same benchmark.exe in both release directories for a controlled comparison'
}
$reports = @()
foreach ($iteration in 1..$Repeats) {
    # Alternate order to reduce bias from warm-up and changing machine load.
    $order = if ($iteration % 2) { @('before', 'after') } else { @('after', 'before') }
    foreach ($variant in $order) {
        $directory = if ($variant -eq 'before') { $BeforeDirectory } else { $AfterDirectory }
        Write-Output "Iteration $iteration : $variant"
        $output = & (Join-Path $PSScriptRoot 'run-native.ps1') -Seconds $Seconds -Repeats 1 -Concurrency $Concurrency -BinaryDirectory $directory -ConfigurationFile $ConfigurationFile -File $File
        $output | Write-Output
        $reportLine = $output | Where-Object { $_ -is [string] -and $_.StartsWith('Report: ') } | Select-Object -Last 1
        if (!$reportLine) { throw 'Native runner did not finish a report' }
        $reportPath = $reportLine.Substring(8)
        $report = Get-Content -Raw -LiteralPath $reportPath | ConvertFrom-Json
        $reports += [pscustomobject]@{ variant = $variant; iteration = $iteration; path = $reportPath; report = $report }
    }
}
function Get-Median($values) {
    $sorted = @($values | Sort-Object)
    $mid = [int][Math]::Floor($sorted.Count / 2)
    if ($sorted.Count % 2) { return $sorted[$mid] }
    return ($sorted[$mid - 1] + $sorted[$mid]) / 2
}
$selectedFiles = if ($File -eq 'all') { @('small.txt', 'medium.txt', 'large.bin') } else { @($File) }
$summary = foreach ($sampleFile in $selectedFiles) {
    $rows = @{}
    foreach ($variant in @('before', 'after')) {
        $samples = @($reports | Where-Object variant -eq $variant | ForEach-Object { $_.report.results } | Where-Object { $_.url.EndsWith("/$sampleFile") })
        $rates = @($samples | ForEach-Object successful_requests_per_second)
        $rows[$variant] = [ordered]@{
            median_requests_per_second = Get-Median $rates
            min_requests_per_second = ($rates | Measure-Object -Minimum).Minimum
            max_requests_per_second = ($rates | Measure-Object -Maximum).Maximum
            median_p99_ms = Get-Median @($samples | ForEach-Object p99_ms)
            errors = ($samples | Measure-Object -Property errors -Sum).Sum
            peak_sampled_working_set_bytes = ($samples | Measure-Object -Property server_peak_sampled_working_set_bytes -Maximum).Maximum
        }
    }
    [ordered]@{
        file = $sampleFile
        before = $rows.before
        after = $rows.after
        throughput_ratio = $rows.after.median_requests_per_second / $rows.before.median_requests_per_second
    }
}
$resultPath = Join-Path $PSScriptRoot ('results/comparison-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '.json')
[ordered]@{
    scope = 'Alternating Windows loopback comparison; same configuration for both builds; same host for client/server; not a cross-server result'
    configuration_file = $ConfigurationFile
    seconds = $Seconds
    repeats = $Repeats
    concurrency = $Concurrency
    summary = @($summary)
    reports = $reports
} | ConvertTo-Json -Depth 15 | Set-Content -LiteralPath $resultPath -Encoding utf8
$summary | ForEach-Object {
    Write-Output ("{0}: {1:N1} -> {2:N1} requests/sec ({3:N2}x); errors {4}/{5}" -f $_.file, $_.before.median_requests_per_second, $_.after.median_requests_per_second, $_.throughput_ratio, $_.before.errors, $_.after.errors)
}
Write-Output "Comparison: $resultPath"
