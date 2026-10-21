# Runs the sample review on Windows and checks the result.
# Usage: .\examples\sample\run.ps1 [-Bin C:\path\to\rv-pdfauth.exe]
param([string]$Bin = "rv-pdfauth")
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$out = Join-Path $here "out"
& $Bin self-test; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
# Expected exit code 3: the sample set contains high-severity findings by design (see examples\README.md).
& $Bin scan --input (Join-Path $here "evidence") --hash sha256,md5 --lang all --case DEMO-001 --actor "Sample" --out $out --progress none
if ($LASTEXITCODE -ne 3) { Write-Error "unexpected exit code $LASTEXITCODE (expected 3)"; exit 1 }
$run = Get-ChildItem (Join-Path $out "logs") | Sort-Object Name | Select-Object -Last 1
& $Bin verify-log $run.FullName; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
# Compare with the independently computed hashes
$expected = Import-Csv (Join-Path $here "expected_hashes.csv")
foreach ($row in $expected) {
  $actual = (Get-FileHash (Join-Path $here "evidence\$($row.file)") -Algorithm SHA256).Hash.ToLower()
  if ($actual -ne $row.sha256) { Write-Error "SHA-256 mismatch for $($row.file)" }
}
Write-Host "Reports: $out\reports\$($run.Name)\statistics.en.html"
