# Launch check for Windows (CI): start Veylo with a fresh, temporary library, let it run, fail if it
# exits or panics, and save a screenshot of the screen.
#   pwsh tools/ci/launch-check.ps1 -Exe <veylo-dx.exe> -Shot <screenshot.png> [-Seconds 25] [-Demo]
# -Demo also plays the test stream at launch (VEYLO_DX_PLAY_DEMO).
param([Parameter(Mandatory)][string]$Exe, [Parameter(Mandatory)][string]$Shot, [int]$Seconds = 25, [switch]$Demo)
$ErrorActionPreference = 'Stop'
$data = Join-Path ([IO.Path]::GetTempPath()) ("veylo-launch-" + [guid]::NewGuid())
New-Item -ItemType Directory -Force -Path $data, (Split-Path -Parent ([IO.Path]::GetFullPath($Shot))) | Out-Null
$env:VEYLO_DX_DATA = $data
if ($Demo) { $env:VEYLO_DX_PLAY_DEMO = '1' } else { Remove-Item Env:VEYLO_DX_PLAY_DEMO -ErrorAction SilentlyContinue }
$panic = Join-Path $data 'last-panic.txt'

$process = Start-Process -FilePath $Exe -PassThru
try {
    Start-Sleep -Seconds $Seconds
    if ($process.HasExited) {
        if (Test-Path $panic) { Get-Content $panic }
        throw "Veylo exited (code $($process.ExitCode)) within $Seconds s."
    }
    if (Test-Path $panic) { Get-Content $panic; throw "Veylo panicked while running." }

    Add-Type -AssemblyName System.Windows.Forms, System.Drawing
    $screen = [System.Windows.Forms.SystemInformation]::VirtualScreen
    $bitmap = New-Object System.Drawing.Bitmap $screen.Width, $screen.Height
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.CopyFromScreen($screen.Left, $screen.Top, 0, 0, $bitmap.Size)
    $bitmap.Save([IO.Path]::GetFullPath($Shot), [System.Drawing.Imaging.ImageFormat]::Png)
    # A screen of one or two colours means nothing was drawn.
    $colours = @{}
    for ($x = 0; $x -lt $bitmap.Width; $x += 16) { for ($y = 0; $y -lt $bitmap.Height; $y += 16) { $colours[$bitmap.GetPixel($x, $y).ToArgb()] = 1 } }
    if ($colours.Count -le 2) { throw "The screenshot is blank ($($colours.Count) colours): the window drew nothing." }
    Write-Host "Screenshot has at least $($colours.Count) colours."
    $graphics.Dispose(); $bitmap.Dispose()
    Write-Host "Veylo ran for $Seconds s; screenshot: $Shot"
} finally {
    if (-not $process.HasExited) { Stop-Process -Id $process.Id -Force }
    Remove-Item -Recurse -Force $data -ErrorAction SilentlyContinue
}
