<#
.SYNOPSIS
  Build the Microsoft Store package (MSIX) for Heroes Without Sound.

.DESCRIPTION
  1. cargo build --release (the standalone, statically-linked deafoverlay.exe)
  2. stage: exe + AppxManifest.xml (tokens filled) + Assets + Geist licence + third-party notices
  3. makeappx pack -> rust/target/msix/HeroesWithoutSound_<version>_x64.msix
  Optional: -Assets regenerates the logo PNGs, -CertPfx signs (sideloading only; the Store
  re-signs submissions itself), -Install registers the staged layout for a local test run
  (needs Windows Developer Mode, no certificate).

.EXAMPLE
  ./build-msix.ps1 -Install
  ./build-msix.ps1 -IdentityName "12345Example.HeroesWithoutSound" -Publisher "CN=ABCD-..." -PublisherDisplay "Heroes Without Sound"
#>
param(
  # Partner Center > Product identity: Package/Identity/Name, .../Publisher, PublisherDisplayName.
  [string]$IdentityName = "HeroesWithoutSound.Dev",
  [string]$Publisher = "CN=HeroesWithoutSound-Dev",
  [string]$PublisherDisplay = "Heroes Without Sound",
  # Defaults to the app crate's version + ".0" (the Store requires the last part to be 0).
  [string]$Version,
  [string]$CertPfx,
  [string]$CertPassword,
  [switch]$Assets,
  [switch]$Install
)
# Windows PowerShell turns any stderr output from native tools (cargo's progress, Chrome's logs)
# into errors, so native calls are checked by exit code and only cmdlets stop on failure.
$ErrorActionPreference = "Continue"
$PSDefaultParameterValues["*:ErrorAction"] = "Stop"
$Pkg = $PSScriptRoot
$Root = Split-Path -Parent $Pkg                     # rust/
$Out = Join-Path $Root "target\msix"
$Stage = Join-Path $Out "stage"

function Find-SdkTool([string]$name) {
  $kits = "C:\Program Files (x86)\Windows Kits\10\bin"
  $hit = Get-ChildItem $kits -Directory -ErrorAction SilentlyContinue | Sort-Object Name -Descending |
    ForEach-Object { Join-Path $_.FullName "x64\$name" } | Where-Object { Test-Path $_ } | Select-Object -First 1
  if (-not $hit) { throw "$name not found: install the Windows SDK (Visual Studio Installer > Windows 11 SDK)." }
  $hit
}

if ($Assets) {
  $chrome = @("C:\Program Files\Google\Chrome\Application\chrome.exe", "C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
  if (-not $chrome) { throw "Chrome or Edge is needed to render the logos." }
  $src = "file:///" + ((Join-Path $Pkg "assets-src\logo.html") -replace "\\", "/" -replace " ", "%20")
  $profile = Join-Path $Out "chrome-profile"
  New-Item -ItemType Directory -Force (Join-Path $Pkg "Assets") | Out-Null
  # Headless Chrome won't go below ~500 px wide, so render at 4x in a big enough window and
  # downscale (which also supersamples the small icons).
  Add-Type -AssemblyName System.Drawing
  $big = Join-Path $Out "logo-4x.png"
  foreach ($a in @(
      @{ n = "Square44x44Logo"; w = 44; h = 44; k = "mark" },
      @{ n = "Square150x150Logo"; w = 150; h = 150; k = "mark" },
      @{ n = "StoreLogo"; w = 50; h = 50; k = "mark" },
      @{ n = "Wide310x150Logo"; w = 310; h = 150; k = "wide" })) {
    $png = Join-Path $Pkg "Assets\$($a.n).png"
    $bw = [math]::Max(600, $a.w * 4); $bh = [int]($bw * $a.h / $a.w)
    Remove-Item $big -ErrorAction SilentlyContinue
    & $chrome --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=1 `
      --default-background-color=00000000 --user-data-dir="$profile" --window-size="$bw,$bh" `
      --virtual-time-budget=2000 --screenshot="$big" "$src#w=$bw&h=$bh&kind=$($a.k)" 2>&1 | Out-Null
    if (-not (Test-Path $big)) { throw "failed to render $($a.n)" }
    $srcImg = [System.Drawing.Image]::FromFile($big)
    $dst = New-Object System.Drawing.Bitmap $a.w, $a.h
    $g = [System.Drawing.Graphics]::FromImage($dst)
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.DrawImage($srcImg, 0, 0, $a.w, $a.h)
    $dst.Save($png, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $dst.Dispose(); $srcImg.Dispose()
    "rendered $($a.n).png"
  }
}

Push-Location $Root
try {
  cmd /c "cargo build --release -p deafoverlay 2>&1" | Out-Host
  if ($LASTEXITCODE) { throw "cargo build failed" }

  if (-not $Version) {
    $line = Select-String -Path (Join-Path $Root "crates\app\Cargo.toml") -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
    $Version = $line.Matches[0].Groups[1].Value + ".0"
  }

  Remove-Item $Stage -Recurse -Force -ErrorAction SilentlyContinue
  New-Item -ItemType Directory -Force $Stage, (Join-Path $Stage "Licenses") | Out-Null
  Copy-Item (Join-Path $Root "target\release\deafoverlay.exe") $Stage
  Copy-Item (Join-Path $Pkg "Assets") (Join-Path $Stage "Assets") -Recurse
  Copy-Item (Join-Path $Root "crates\app\fonts\OFL.txt") (Join-Path $Stage "Licenses\Geist-OFL.txt")

  $manifest = (Get-Content (Join-Path $Pkg "AppxManifest.xml") -Raw).
    Replace("{IDENTITY_NAME}", $IdentityName).Replace("{PUBLISHER}", $Publisher).
    Replace("{PUBLISHER_DISPLAY}", $PublisherDisplay).Replace("{VERSION}", $Version)
  Set-Content (Join-Path $Stage "AppxManifest.xml") $manifest -Encoding utf8

  # Third-party notices: every crate compiled into the exe, with its licence files.
  $meta = cargo metadata --format-version 1 --manifest-path (Join-Path $Root "Cargo.toml") | ConvertFrom-Json
  $inExe = cargo tree -p deafoverlay -e normal --prefix none --target x86_64-pc-windows-msvc |
    ForEach-Object { if ($_ -match '^(\S+) v(\S+)') { "$($Matches[1]) $($Matches[2])" } } | Sort-Object -Unique
  $notes = [System.Text.StringBuilder]::new()
  [void]$notes.AppendLine("Heroes Without Sound includes the following third-party software.`r`n")
  [void]$notes.AppendLine("Geist and Geist Mono fonts: SIL Open Font License 1.1, see Licenses\Geist-OFL.txt.`r`n")
  foreach ($p in $meta.packages | Where-Object { $_.source -and ($inExe -contains "$($_.name) $($_.version)") } | Sort-Object name) {
    [void]$notes.AppendLine(("=" * 72))
    [void]$notes.AppendLine("$($p.name) $($p.version)  ($($p.license))")
    if ($p.repository) { [void]$notes.AppendLine($p.repository) }
    $dir = Split-Path -Parent $p.manifest_path
    foreach ($lf in Get-ChildItem $dir -File | Where-Object { $_.Name -match '^(LICEN[CS]E|COPYING|NOTICE)' }) {
      [void]$notes.AppendLine("`r`n--- $($lf.Name) ---")
      [void]$notes.AppendLine((Get-Content $lf.FullName -Raw))
    }
  }
  Set-Content (Join-Path $Stage "Licenses\ThirdPartyNotices.txt") $notes.ToString() -Encoding utf8

  $msix = Join-Path $Out "HeroesWithoutSound_$($Version)_x64.msix"
  & (Find-SdkTool "makeappx.exe") pack /o /d $Stage /p $msix | Out-Null
  if ($LASTEXITCODE) { throw "makeappx failed" }
  if ($CertPfx) {
    & (Find-SdkTool "signtool.exe") sign /fd SHA256 /f $CertPfx /p $CertPassword $msix
    if ($LASTEXITCODE) { throw "signtool failed" }
  }
  "packed $msix ($([math]::Round((Get-Item $msix).Length / 1MB, 2)) MB, version $Version)"

  if ($Install) {
    Get-AppxPackage -Name $IdentityName | Remove-AppxPackage
    Add-AppxPackage -Register (Join-Path $Stage "AppxManifest.xml")
    "registered $IdentityName from $Stage (Developer Mode test install)"
  }
} finally {
  Pop-Location
}
