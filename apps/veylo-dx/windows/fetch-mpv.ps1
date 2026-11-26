# Fetch libmpv for the Windows desktop build from a pinned shinchiro/mpv-winbuild-cmake release:
# libmpv-2.dll (mpv + FFmpeg), its import library and headers. Nothing is committed:
# windows/mpv/ is ignored; build.rs links it and copies the DLL next to veylo-dx.exe.
# Licences: libmpv and FFmpeg as shipped by that release (GPLv2+ build).
# Run from any folder:  powershell -ExecutionPolicy Bypass -File apps\veylo-dx\windows\fetch-mpv.ps1
$ErrorActionPreference = 'Stop'
$Tag = '20261111'
$Asset = 'mpv-dev-x86_64-20261111-git-bcde6b93cc.7z'
$Sha256 = '273c079b11ea5575822f972756f4c9a71149a5c10130353abeb8586738938452'

$out = Join-Path $PSScriptRoot 'mpv'
$tagFile = Join-Path $out 'TAG'
if ((Test-Path $tagFile) -and ((Get-Content -Raw $tagFile).Trim() -eq $Tag) `
    -and (Test-Path (Join-Path $out 'libmpv-2.dll')) -and (Test-Path (Join-Path $out 'mpv.lib'))) {
    Write-Host "Windows libmpv $Tag already present."
    exit 0
}
$work = Join-Path ([IO.Path]::GetTempPath()) ("veylo-mpv-" + [guid]::NewGuid())
New-Item -ItemType Directory -Force -Path $work, $out | Out-Null
try {
    $archive = Join-Path $work $Asset
    $uri = "https://github.com/shinchiro/mpv-winbuild-cmake/releases/download/$Tag/$Asset"
    $downloaded = $false
    for ($attempt = 1; $attempt -le 5; $attempt++) {
        try {
            Invoke-WebRequest -UseBasicParsing -Uri $uri -OutFile $archive
            $downloaded = $true
            break
        } catch {
            if ($attempt -eq 5) { throw }
            $wait = [Math]::Min([Math]::Pow(2, $attempt), 30)
            Write-Host "Fetch attempt $attempt failed ($($_.Exception.Message)); retrying in ${wait}s..."
            Start-Sleep -Seconds $wait
        }
    }
    if (-not $downloaded) { throw "Failed to download $Asset" }
    $hash = (Get-FileHash -Algorithm SHA256 $archive).Hash.ToLower()
    if ($hash -ne $Sha256) { throw "Checksum mismatch for ${Asset}: $hash" }

    # 7-Zip if installed, otherwise the tar.exe that ships with Windows 10 1803+.
    $sevenZip = @((Get-Command 7z -ErrorAction SilentlyContinue).Source, "$env:ProgramFiles\7-Zip\7z.exe") |
        Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
    if ($sevenZip) { & $sevenZip x -y "-o$out" $archive | Out-Null }
    else { tar -xf $archive -C $out }
    if ($LASTEXITCODE -ne 0) { throw "Couldn't extract $Asset (install 7-Zip and run again)" }

    # MSVC links against mpv.lib; the release's import library serves both toolchains.
    Copy-Item -Force (Join-Path $out 'libmpv.dll.a') (Join-Path $out 'mpv.lib')
    Set-Content -Path (Join-Path $out 'TAG') -Value $Tag
    Get-ChildItem $out | Select-Object Name, Length
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
