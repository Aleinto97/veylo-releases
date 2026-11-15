# Veylo for Windows

A quiet media client: Home, Picks that learn your taste, a Library, and an mpv player.

## Install

Open **PowerShell** or **Windows Terminal** and run:

```powershell
irm https://raw.githubusercontent.com/Aleinto97/veylo-releases/main/install.ps1 | iex
```

Veylo installs to `%LOCALAPPDATA%\Programs\Veylo` (no admin rights) and adds Start menu and desktop shortcuts.
Windows 10 or 11, 64-bit.

## Updates

When a new version is out, Veylo shows **Veylo x.y is available** with an **Update** button. Press it and Veylo
downloads the new version and restarts. Running the install command again also updates.

## Uninstall

Delete `%LOCALAPPDATA%\Programs\Veylo` and the two Veylo shortcuts. Your library is in `%LOCALAPPDATA%\veylo-dx`.

This repository holds only the Windows builds. The player is [libmpv](https://mpv.io) built by
[mpv-winbuild-cmake](https://github.com/shinchiro/mpv-winbuild-cmake) (source and licences there).
