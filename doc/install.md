# Install and try SeaSnail

[简体中文](install.zh-CN.md)

SeaSnail currently targets macOS 13+ on Apple Silicon. Intel Macs, Windows, and Linux are not supported by the native packaging workflow.

Check the specific [Release](https://github.com/0Litost0/SeaSnail/releases) for available assets, checksums, signature status, and known issues. If there is no published build, use the [source development guide](development.md); do not assume a release exists.

1. Download the macOS arm64 archive from a trusted release and verify its SHA-256 against the release's checksum file (`shasum -a 256 <downloaded-file>`).
2. Unzip, move SeaSnail.app to Applications, and open it. For an unsigned build that macOS blocks, use the system's explicit approval flow only after checking the origin. Follow the instructions for your macOS version and that release; a checksum alone does not establish publisher identity.
3. Enter a local username and password, then choose **Review privacy and permissions**. This step does not create the account or access Keychain. Keep the password safe.
4. Read the Keychain explanation, then choose **Create account and enable secure storage**. macOS may ask to access `com.seasnail` Keychain entries. Microphone and Accessibility have separate authorization buttons; you can defer those permissions and configure them later.
5. Review the shortcut and clipboard-context guide. Open a text editor, place the cursor, press **⌘ ⇧ Space**, speak, and press again to stop. If automatic pasting is unavailable, use the clipboard or session history.

The repository's current build scripts produce unsigned development packages with a file Keychain fallback. They are intended for local/internal testing; read [privacy](privacy.md) and the [release checklist](releasing.md). Do not describe such a package as a signed production release.

## Troubleshooting

- Cannot record: check System Settings → Privacy & Security → Microphone.
- Cannot auto-paste: check Accessibility in the same settings section; try manual paste or history.
- Shortcut conflict: change Settings → General → recording shortcut.
- Failed transcription: retain the session for retry and inspect redacted logs; never post its contents publicly.
- Logs: by default `~/Library/Application Support/SeaSnail/logs/`. Redact paths and personal data before sharing.

## Uninstall

Quit SeaSnail before removing the app. Removing the app does not remove encrypted account data. To permanently remove all local history, first export anything you need, then remove `~/Library/Application Support/SeaSnail/` (or your custom `SEASNAIL_DATA_DIR`) yourself. This deletion is irreversible. Keychain records are separate; signing identities and development fallback locations vary, so do not delete unrelated Keychain entries.
