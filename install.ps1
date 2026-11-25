# Veylo for Windows: install, or update to, the latest release. In PowerShell or Windows Terminal:
#   irm https://raw.githubusercontent.com/Aleinto97/veylo-releases/main/install.ps1 | iex
# Installs to %LOCALAPPDATA%\Programs\Veylo (no admin rights) with Start menu and desktop shortcuts.
# Later versions arrive through the Update button inside Veylo.
# Testing: $env:VEYLO_ZIP = <local zip> installs that file; $env:VEYLO_NO_LAUNCH = 1 doesn't open the app.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$repo = 'Aleinto97/veylo-releases'
$assetName = 'veylo-windows-x64.zip'
$dest = Join-Path $env:LOCALAPPDATA 'Programs\Veylo'
$exe = Join-Path $dest 'veylo-dx.exe'

if ($env:VEYLO_ZIP) {
    $label = 'from ' + (Split-Path -Leaf $env:VEYLO_ZIP)
} else {
    $release = Invoke-RestMethod -UseBasicParsing -Uri "https://api.github.com/repos/$repo/releases/latest" -Headers @{ 'User-Agent' = 'veylo-installer' }
    $asset = $release.assets | Where-Object { $_.name -eq $assetName } | Select-Object -First 1
    if (-not $asset) { throw "The latest Veylo release has no Windows download yet." }
    $label = $release.tag_name
}
Write-Host "Installing Veylo $label..."

# A running Veylo from this folder holds its files: close it first.
Get-Process veylo-dx -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe } | Stop-Process -Force
Start-Sleep -Milliseconds 500

$zip = Join-Path ([IO.Path]::GetTempPath()) $assetName
if ($env:VEYLO_ZIP) {
    Copy-Item -Force $env:VEYLO_ZIP $zip
} else {
    $sumsAsset = $release.assets | Where-Object { $_.name -eq 'SHA256SUMS.txt' } | Select-Object -First 1
    if (-not $sumsAsset) { throw "Refusing to install: SHA256SUMS.txt is missing from the release." }
    $sumsText = (Invoke-WebRequest -UseBasicParsing -Uri $sumsAsset.browser_download_url).Content
    $expected = $null
    foreach ($line in ($sumsText -split "`r?`n")) {
        $parts = $line.Trim() -split '\s+'
        if ($parts.Length -ge 2 -and $parts[1].TrimStart('*') -eq $assetName) {
            $expected = $parts[0].ToLowerInvariant()
            break
        }
    }
    if (-not $expected) { throw "Refusing to install: $assetName checksum not found in SHA256SUMS.txt." }
    Invoke-WebRequest -UseBasicParsing -Uri $asset.browser_download_url -OutFile $zip
    $actual = (Get-FileHash -Algorithm SHA256 -Path $zip).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        Remove-Item -Force $zip -ErrorAction SilentlyContinue
        throw "Refusing to install: SHA-256 verification failed for $assetName."
    }
}
New-Item -ItemType Directory -Force -Path $dest | Out-Null
Expand-Archive -Force -Path $zip -DestinationPath $dest
Remove-Item -Force $zip

# Register per-user stremio:// and veylo:// deep-link protocol handlers (no admin rights needed).
foreach ($scheme in @('stremio', 'veylo')) {
    $baseKey = "HKCU:\Software\Classes\$scheme"
    New-Item -Path "$baseKey\shell\open\command" -Force | Out-Null
    Set-ItemProperty -Path $baseKey -Name '(Default)' -Value "URL:$scheme Protocol"
    Set-ItemProperty -Path $baseKey -Name 'URL Protocol' -Value ''
    Set-ItemProperty -Path "$baseKey\shell\open\command" -Name '(Default)' -Value "`"$exe`" `"%1`""
}

$shell = New-Object -ComObject WScript.Shell
foreach ($folder in @([Environment]::GetFolderPath('Programs'), [Environment]::GetFolderPath('Desktop'))) {
    $link = $shell.CreateShortcut((Join-Path $folder 'Veylo.lnk'))
    $link.TargetPath = $exe
    $link.WorkingDirectory = $dest
    $link.Save()
}
if ($env:VEYLO_NO_LAUNCH) {
    Write-Host "Veylo $label is installed in $dest."
} else {
    Write-Host "Veylo $label is installed. Opening it now; next time use the Start menu or desktop shortcut."
    Start-Process $exe
}
