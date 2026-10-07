# Privacy and permissions

[简体中文](privacy.zh-CN.md)

Speech recognition runs locally. SeaSnail stores account databases using SQLCipher and encrypts audio, transcripts, clipboard context, and cleanup artifacts on disk. Local accounts do not register with an online service. Logging out preserves encrypted data and clears automatic-login credentials; logging back in requires the password.

## System permissions

| Permission | Purpose | Without permission | macOS location |
| --- | --- | --- | --- |
| Keychain | Store the local account encryption key and sign-in credential under `com.seasnail` | Secure account setup cannot complete | Keychain Access → search for `com.seasnail` |
| Microphone | Capture audio when you start dictation, then transcribe on-device | Microphone dictation cannot run | System Settings → Privacy & Security → Microphone → SeaSnail |
| Accessibility | Paste text into the focused application; support observing subsequent edits for dictionary learning | Automatic pasting and related edit learning are unavailable; use clipboard/history | System Settings → Privacy & Security → Accessibility → SeaSnail |

The account form only collects your details. Account creation and Keychain access begin after you read the explanation and click **Create account and enable secure storage** on the privacy page. macOS may show a Keychain access dialog at that point; its appearance depends on signing identity and existing Keychain access rules.

Microphone and Accessibility are requested separately using their authorization buttons. Checking their status does not itself request permission. If denied, enable the permission in System Settings and return to SeaSnail to check again.

## Clipboard context and dictionary learning

Clipboard capture is enabled by default and can be changed during setup or in Settings → Privacy & permissions. Capture is active during recording. Text, rich text, links, files, and images may be recorded as context. Files and images appear as local paths in combined text. Do not copy secrets while context capture is active.

Automatic dictionary learning is also configurable in Settings → Privacy & permissions. It can use validated cleanup corrections and changes you make to automatically pasted text. Personal dictionary entries stay in the local account; when cleanup is enabled, the task's dictionary spelling hints are included in provider input. The dictionary is not currently used as local ASR hints.

## Optional remote cleanup

Cleanup is off by default. When you configure a provider and enable cleanup, the transcript and dictionary hints are sent to that provider. Actual clipboard payloads are replaced with request-specific opaque placeholders and restored locally only after validation. The provider's own data handling policy applies to the text it receives. A 15-second live deadline and output validation protect the fallback path: failed cleanup uses the original transcript.

## Exports and development builds

Exports and clipboard text can contain plaintext sensitive content. Save and share them carefully. The WebView does not receive raw audio, the root bearer, or plaintext export ZIP bytes.

Current unsigned development packages carry `SEASNAIL_DEV_FILE_KEYCHAIN`, enabling a file-based fallback for local testing. That fallback does not have the protection of macOS Keychain and must be resolved before public binary distribution. Do not treat disk encryption as protection against malware running under your OS account.
