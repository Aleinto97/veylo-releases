# Veylo accounts (Firebase)

Sign-in and sync between a person's devices (`src/backend/account.rs`, Settings → Account) use a
Firebase project: Authentication (email/password, Google, anonymous for a computer waiting on its
QR code) and Firestore (`firestore.rules` here: paste them under Firestore Database → Rules).

A build gets the project from these environment variables at compile time. Release builds take
them from the repository's Actions secrets; never commit them:

| Variable | Where it comes from |
|---|---|
| `VEYLO_FIREBASE_API_KEY` | Project settings → General → Web API Key |
| `VEYLO_FIREBASE_PROJECT` | Project settings → General → Project ID |
| `VEYLO_GOOGLE_WEB_CLIENT` | Authentication → Sign-in method → Google → Web SDK configuration → Web client ID |
| `VEYLO_GOOGLE_DESKTOP_CLIENT` | Google Cloud → APIs & Services → Credentials → OAuth client "Desktop app" → Client ID |
| `VEYLO_GOOGLE_DESKTOP_SECRET` | the same client's secret |

Without them the app has no Account section. Android's Google sign-in also needs the release
key's SHA-1 and SHA-256 registered on the project's Android app (`dev.veylo.dx`).
