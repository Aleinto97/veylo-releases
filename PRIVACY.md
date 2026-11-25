# Veylo Privacy Policy

Last updated: 2026-11-25

Veylo is designed to be local-first and privacy-respecting. You can use Veylo
without creating an account.

## 1. Data Stored Locally on Your Device

Veylo stores your library, watch progress, preferences, search history, learned
Picks taste weights, and user-installed add-on URLs locally in your app data
folder (`0700` directory / `0600` files on Unix; user-restricted ACLs on Windows).

- **On-device AI Picks**: Recommendations run locally on your CPU via ONNX
  Runtime. Your viewing history and taste profile are never sent to any AI cloud.
- **Credentials & add-on links**: Masked in the UI, never printed in logs or
  errors, and automatically stripped from diagnostics.

## 2. Optional Account & End-to-End Encrypted Cloud Sync

If you choose to sign in (Email/Password, Google Sign-In, or QR device linking)
using Firebase Authentication & Firestore:

- **Zero-Knowledge Encryption (E2EE)**: Your synced watch history, episode
  progress, saved titles, likes, preferences, and add-on URLs are
  encrypted on your device with AES-256-GCM using a randomly generated 256-bit
  Sync Key that never leaves your devices in plaintext. Neither Firebase nor the
  project operator can decrypt your synced library, keys, or add-ons.
- **QR Device Linking**: Transfers your session and Sync Key directly between
  devices using a single-use random 256-bit ephemeral AES-256-GCM key embedded
  in the QR code; the temporary Firestore link document is deleted immediately
  after pickup.
- **Account & Data Deletion**: You can export your watch history to JSON or
  permanently delete your account and all cloud-synced documents at any time
  under **Settings → Account → Delete account**.

## 3. Diagnostics & Crash Reporting (Strictly Opt-In)

- Automatic issue and crash reporting is **OFF by default**.
- If you explicitly turn it on in Settings, or submit a manual bug report, Veylo
  sends only generic OS/architecture tags (never computer hostnames or device
  serials), app version, display HDR capability, and redacted logs with URLs,
  API keys, tokens, emails, and user home paths stripped.

## 4. Third-Party Services & User-Installed Add-ons

Veylo connects directly from your device to:
- **Metadata & artwork providers** you enable (Cinemeta, BetterPosters, TVmaze,
  Wikidata/Wikimedia/Wikipedia, AniList, ElfHosted, OpenSubtitles /
  IntroDB / AniSkip during playback).
- **User-installed add-ons**: Veylo ships with no third-party streaming add-ons.
  Any add-on URL you install is contacted directly by your device according to
  that add-on's own privacy policy. By default, Veylo does **not** prefetch
  streams while browsing titles; stream add-ons are queried only when you press
  Play (unless you enable browsing prefetch in Settings).
- **Update checks**: Desktop/Android builds check the public GitHub Releases API
  for newer versions and require verified SHA-256 checksums before installing.

## 5. Your Rights (GDPR / CCPA)

You have full control over your data:
- **Access & Portability**: Export your library and add-on bundle from Settings.
- **Deletion**: Delete local data by removing Veylo's data folder; delete all
  cloud data anytime via **Settings → Account → Delete account**.
- **Consent**: Telemetry and cloud sync are strictly optional and off by default.
