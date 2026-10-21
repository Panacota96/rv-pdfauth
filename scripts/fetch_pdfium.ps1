# Downloads the pinned PDFium build (third_party/pdfium/PDFIUM_VERSION.txt), verifies its SHA-256 and
# unpacks the library + licences into third_party/pdfium/<platform>/ (git-ignored).
#
#   .\scripts\fetch_pdfium.ps1                    # win-x64 -> third_party\pdfium\win-x64\pdfium.dll
#   .\scripts\fetch_pdfium.ps1 -Platform linux-x64
#
# Then: $env:RV_PDFIUM_PATH = "$PWD\third_party\pdfium\win-x64\pdfium.dll"
param([string]$Platform = "win-x64")
$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$pin = Join-Path $root "third_party\pdfium\PDFIUM_VERSION.txt"
$values = @{}
foreach ($line in Get-Content $pin) {
    if ($line -match '^\s*#' -or $line -notmatch '=') { continue }
    $k, $v = $line -split '=', 2
    $values[$k.Trim()] = $v.Trim()
}
$tag = $values["tag"]
$expected = $values["sha256.$Platform"]
if (-not $tag -or -not $expected) { Write-Error "no tag or sha256.$Platform in $pin"; exit 2 }

switch -Wildcard ($Platform) {
    "win-*" { $member = "bin/pdfium.dll"; $lib = "pdfium.dll" }
    "mac-*" { $member = "lib/libpdfium.dylib"; $lib = "libpdfium.dylib" }
    default { $member = "lib/libpdfium.so"; $lib = "libpdfium.so" }
}

$dest = Join-Path $root "third_party\pdfium\$Platform"
$stamp = Join-Path $dest "ARCHIVE_SHA256"
if ((Test-Path (Join-Path $dest $lib)) -and (Test-Path $stamp) -and ((Get-Content $stamp -Raw).Trim() -eq $expected)) {
    Write-Output (Join-Path $dest $lib)
    exit 0
}

$work = Join-Path ([IO.Path]::GetTempPath()) ("pdfium-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Force $work | Out-Null
try {
    $url = "https://github.com/bblanchon/pdfium-binaries/releases/download/$($tag -replace '/', '%2F')/pdfium-$Platform.tgz"
    Write-Host "downloading $url"
    $tgz = Join-Path $work "pdfium.tgz"
    Invoke-WebRequest -Uri $url -OutFile $tgz -UseBasicParsing
    $actual = (Get-FileHash -Algorithm SHA256 $tgz).Hash.ToLowerInvariant()
    if ($actual -ne $expected) { Write-Error "SHA-256 mismatch for pdfium-${Platform}.tgz: expected $expected, got $actual"; exit 1 }
    $x = Join-Path $work "x"
    New-Item -ItemType Directory -Force $x | Out-Null
    # tar.exe ships with Windows 10 1803+.
    tar -xzf $tgz -C $x $member LICENSE VERSION licenses
    if ($LASTEXITCODE -ne 0) { Write-Error "tar failed"; exit 1 }
    if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
    New-Item -ItemType Directory -Force $dest | Out-Null
    Copy-Item (Join-Path $x $member) (Join-Path $dest $lib)
    Copy-Item (Join-Path $x "LICENSE") (Join-Path $dest "LICENSE-pdfium-binaries.txt")
    Copy-Item (Join-Path $x "VERSION") (Join-Path $dest "VERSION")
    Copy-Item -Recurse (Join-Path $x "licenses") (Join-Path $dest "licenses")
    Set-Content -NoNewline -Path $stamp -Value $expected
    Write-Output (Join-Path $dest $lib)
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
