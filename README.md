# Veylo for Windows and Android

A quiet media client: Home, Picks that learn your taste, a Library, and an mpv player.

## Install

Open **PowerShell** or **Windows Terminal** and run:

```powershell
irm https://raw.githubusercontent.com/Aleinto97/veylo-releases/main/install.ps1 | iex
```

Veylo installs to `%LOCALAPPDATA%\Programs\Veylo` (no admin rights) and adds Start menu and desktop shortcuts.
Windows 10 or 11, 64-bit.

## Install on Android

On the phone or tablet (64-bit Android 8 or later), open:

https://github.com/Aleinto97/veylo-releases/releases/latest/download/veylo-android-arm64.apk

Open the downloaded file and allow your browser to install apps when Android asks.

## Updates

When a new version is out, Veylo shows **Veylo x.y is available** with an **Update** button.

- **Windows:** press it and Veylo downloads the new version and restarts. Running the install command again also updates.
- **Android:** press it and Android asks "Update this app?". The first time, it asks you to allow Veylo to install
  apps: allow it, come back and press Update again.

## Uninstall

Delete `%LOCALAPPDATA%\Programs\Veylo` and the two Veylo shortcuts. Your library is in `%LOCALAPPDATA%\veylo-dx`.

This repository holds only the Windows and Android builds. The player is [libmpv](https://mpv.io), built by
[mpv-winbuild-cmake](https://github.com/shinchiro/mpv-winbuild-cmake) for Windows and
[mpv-android](https://github.com/mpv-android/mpv-android) for Android (source and licences there).
