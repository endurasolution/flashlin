$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
$fixturePath = Join-Path $projectRoot 'target/benchmark-fixture'
New-Item -ItemType Directory -Force -Path $fixturePath | Out-Null
# Deterministic fixtures; this script owns only these three files.
foreach ($sample in @(@('small.txt', 1024), @('medium.txt', 65536), @('large.bin', 4194304))) {
    $bytes = [byte[]]::new($sample[1])
    for ($index = 0; $index -lt $bytes.Length; $index++) { $bytes[$index] = 65 + ($index % 26) }
    [IO.File]::WriteAllBytes((Join-Path $fixturePath $sample[0]), $bytes)
}
Get-ChildItem -LiteralPath $fixturePath | Get-FileHash -Algorithm SHA256
