# Releasing

`\.github/workflows/release.yml` builds macOS (arm64 + x86_64), Windows and Linux
bundles on a `v*` tag, attaches them to a draft GitHub release, and publishes
the same artefacts to R2.

```
git tag v1.0.1 && git push origin v1.0.1
```

## Where the downloads live

GitHub releases carry the raw bundles; the artefacts people are pointed at live
on R2, because that is where versioned objects, stable aliases and a CDN are:

| | |
|---|---|
| Bucket | `wired-terminal-releases` |
| Public base | <https://wired-terminal-releases.wired.dev> |
| Manifest | `updates/latest.json` |
| Installers | `downloads/wired-terminal_<version>_<target>.zip` |
| Stable aliases | `downloads/<target>.zip` |

Targets are `macos-aarch64`, `macos-x86_64`, `windows-x86_64` and
`linux-x86_64` — one per build, because macOS here is two builds rather than a
universal one.

`.github/scripts/publish-r2.mjs` does the upload, and
<https://terminal.wired.dev> reads `latest.json` to decide whether it has
downloads to offer. No manifest means the site says builds are not published
yet, so nothing has to be switched on there when the first release lands.

The manifest carries two maps and they are not the same thing:

- **`platforms`** is Tauri's updater contract — a signed URL per platform. It
  is empty until updater signing is wired up (below), and an empty one is a
  warning rather than a failure, because refusing to publish would mean no
  downloads at all.
- **`downloads`** is ours: target → URL, so the site links installers instead
  of reconstructing filenames. Renaming an artefact here cannot silently 404
  there.

Credentials: `CLOUDFLARE_ACCOUNT_ID` plus either `CLOUDFLARE_API_TOKEN`
(Account → Workers R2 Storage → Edit, used via Wrangler) or the
`R2_ACCESS_KEY_ID` / `R2_SECRET_ACCESS_KEY` pair (R2 → Object Read & Write, used
via the S3 API). The job checks for them before downloading a single artefact.

The bucket also needs a CORS rule allowing `GET` from the site's origin, or the
page's fetch fails and the tab looks exactly like a product with no release:

```bash
wrangler r2 bucket cors set wired-terminal-releases \
  --file ../wired-terminal-landing/scripts/r2-cors.json
```

To publish without cutting a tag: **Actions → Release → Run workflow**, give it
the tag to build and leave *Publish installers + latest.json to R2* ticked.

The workflow runs unsigned when the signing secrets are absent, so a fork can
still produce artefacts. Everything below is about making those artefacts open
without a scary dialog.

## Why signing matters more than it looks

An unsigned macOS build does not say "this developer is unknown". It says
**"Wired Terminal is damaged and can't be opened"**, which reads as malware
rather than as a missing certificate, and a non-coder will delete it. Budget for
the Apple Developer account (US$99/year, and the enrolment itself can take days)
before promising anyone a download link.

## macOS: signing and notarization

Needs an **Apple Developer ID Application** certificate.

1. Enrol at <https://developer.apple.com/programs/>.
2. In the developer portal, create a *Developer ID Application* certificate and
   download it.
3. Export it from Keychain Access as a `.p12` with a password.
4. Base64-encode it: `base64 -i certificate.p12 | pbcopy`.
5. Create an app-specific password at <https://appleid.apple.com> for
   notarization.

Then add these repository secrets:

| Secret | What it is |
|---|---|
| `APPLE_CERTIFICATE` | the base64 `.p12` from step 4 |
| `APPLE_CERTIFICATE_PASSWORD` | the password from step 3 |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | your Apple ID email |
| `APPLE_PASSWORD` | the app-specific password from step 5 |
| `APPLE_TEAM_ID` | the 10-character team id |

`tauri-action` picks all six up automatically. Nothing in
`tauri.conf.json` needs changing.

## Windows: signing, or the SmartScreen path

An OV or EV code-signing certificate removes the "Windows protected your PC"
dialog. Set `WINDOWS_CERTIFICATE` (base64 `.pfx`) and
`WINDOWS_CERTIFICATE_PASSWORD`.

Without one, [Troubleshooting](troubleshooting.md#windows-says-windows-protected-your-pc)
documents the two clicks. Expect antivirus false positives as well: an unsigned
binary that installs other programs is exactly the shape heuristic scanners flag,
and the support load is real.

## Windows: verify before shipping

The POSIX-only paths in `providers.rs` have been fixed — `which` now looks for
`.exe`, `.cmd` and `.bat`, `resolve_shell` returns PowerShell, and `widen_path`
in the desktop shell splits on `;`. That is not the same as *tested*. Run the
Windows artefact through [the acceptance list](#acceptance) before advertising
Windows support, or ship macOS-first and say so on the download page.

## Automatic updates

Not wired up yet: the Tauri updater plugin refuses to build without a minisign
public key in `tauri.conf.json`, and the matching private key is a release
secret that cannot live in a repository. The release workflow already passes
`TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` to the
build, and the publish job already uploads `.sig` files and fills in
`platforms` the moment they exist — so what is left is steps 1–4:

1. `npm run tauri signer generate -- -w ~/.tauri/wired.key`
2. Add `tauri-plugin-updater` to `app/src-tauri/Cargo.toml` and
   `.plugin(tauri_plugin_updater::Builder::new().build())` in `lib.rs`.
3. Add to `tauri.conf.json`:
   ```json
   "plugins": {
     "updater": {
       "endpoints": ["https://wired-terminal-releases.wired.dev/updates/latest.json"],
       "pubkey": "<the public key from step 1>"
     }
   }
   ```
   and `"createUpdaterArtifacts": true` under `bundle`.
4. Add `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` as
   repository secrets. The first is already set; the password is not, and the
   build fails on a key that expects one.

Until then, a non-coder keeps whatever version they installed forever, which is
the single strongest argument for doing this before a public launch.

## Acceptance

Before calling a release good, on a machine with no developer tools:

- [ ] Download → open → the app runs, with no security dialog you cannot get
      past in two clicks
- [ ] The wizard installs an assistant CLI and signs it in, with nothing typed
      into a terminal
- [ ] A message sent from the app gets an answer
- [ ] The window closes, the tray icon stays, a scheduled task still fires
- [ ] Pairing a phone over Telegram works end to end, including the Allow button
- [ ] **Help → Copy diagnostics** produces something worth pasting into a support
      thread
