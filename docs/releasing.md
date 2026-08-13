# Releasing

`\.github/workflows/release.yml` builds macOS (arm64 + x86_64), Windows and Linux
bundles on a `v*` tag, attaches them to a draft GitHub release, and publishes
the same artefacts to R2.

```
git tag v1.0.4 && git push origin v1.0.4
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
| Server binaries | `downloads/wired-terminal-server_<version>_linux-x86_64.tar.gz` |
| Server alias | `downloads/linux-x86_64-server.tar.gz` |

Targets are `macos-aarch64`, `macos-x86_64`, `windows-x86_64` and
`linux-x86_64` — one per build, because macOS here is two builds rather than a
universal one.

`.github/scripts/publish-r2.mjs` does the upload, and
<https://terminal.wired.dev> reads `latest.json` to decide whether it has
downloads to offer. No manifest means the site says builds are not published
yet, so nothing has to be switched on there when the first release lands.

The server tarball is the odd one out, because `tauri-action` does not build it.
It bundles a desktop app; a headless install wants the two binaries themselves,
and unpacking a `.deb` to fish one out is not an install path. So `release.yml`
runs a plain `cargo build --release` on the Linux runner and ships
`wired-backend` + `wired` flat in a `.tar.gz`. Without it,
`scripts/install-ubuntu.sh` has nothing to download and every server install
adds apt and rustup and compiles Rust on the target box — which is why the smoke
check at the end of the publish job **fails the release** when that entry goes
missing, rather than letting the toolchain quietly come back.

The manifest carries three maps and they are not the same thing:

- **`platforms`** is Tauri's updater contract — a minisign-signed URL per
  platform, which is what the app's **Install and restart** verifies before it
  replaces itself. Empty is still a warning rather than a failure, because
  refusing to publish would mean no downloads at all; it means the signing broke
  and the app has nothing it will accept.
- **`downloads`** is ours: target → URL, so the site links installers instead
  of reconstructing filenames. Renaming an artefact here cannot silently 404
  there.
- **`server`** is the headless pair, kept apart because a `.deb` and a systemd
  install are not the same download. `install-ubuntu.sh` never reads this map —
  it `curl`s the stable alias, so it needs no JSON parser and no `jq` on a box
  that has nothing installed yet. The map is for anyone who wants the exact
  version rather than the newest.

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

## Releases are unsigned, deliberately

There is no Apple Developer account (US$99/year) and no Windows code-signing
certificate behind this project, and neither is planned. Nothing in the workflow
reads a certificate secret, so a fork needs none either. What that costs is one
dialog per platform, and both are documented for users in
[Troubleshooting](troubleshooting.md):

| | What the user sees | Way past it |
|---|---|---|
| macOS | "Apple could not verify…" | **Open Anyway** in Privacy & Security |
| Windows | "Windows protected your PC" | **More info** → **Run anyway** |

Expect antivirus false positives on Windows as well: an unsigned binary that
installs other programs is exactly the shape heuristic scanners flag, and the
support load is real.

### Ad-hoc signing is free, and it is not optional

*Unsigned* and *ad-hoc signed* sound like the same thing and are not. Skip the
ad-hoc step and the only signature in the bundle is the one the linker puts on
the inner binary, whose CodeDirectory declares sealed resources that no
`_CodeSignature/CodeResources` provides:

```
$ codesign --verify --strict -vvv "/Applications/Wired Terminal.app"
… code has no resources but signature indicates they must be present
```

Gatekeeper reads that as corruption rather than as a missing certificate, and
says **"Wired Terminal is damaged and can't be opened. You should move it to the
Bin"** — the one verdict with no **Open Anyway** button, so the two clicks in the
table above are not reachable and a non-coder deletes the app. v1.0.1 shipped in
that state.

`release.yml` therefore passes `APPLE_SIGNING_IDENTITY: '-'` — ad-hoc, meaning no
certificate, no keychain and no cost, but a self-consistent signature. That is
what turns "damaged" into a dialog with a way out. It is a literal rather than a
secret lookup on purpose: Tauri checks whether these variables are *set*, not
whether they hold a value, so an empty one is worse than an absent one.

## Windows: verify before shipping

The POSIX-only paths in `providers.rs` have been fixed — `which` now looks for
`.exe`, `.cmd` and `.bat`, `resolve_shell` returns PowerShell, and `widen_path`
in the desktop shell splits on `;`. That is not the same as *tested*. Run the
Windows artefact through [the acceptance list](#acceptance) before advertising
Windows support, or ship macOS-first and say so on the download page.

## Automatic updates

Both halves update themselves in place. Neither downloads a file for someone to
open, which is what they used to do — the app opened a browser at the `.dmg` and
the CLI printed a URL, and "there is a new version, go and fetch it" is not an
update.

**The desktop app** uses `tauri-plugin-updater`, signed with the minisign key
shared across the Wired products (`~/.tauri/wired.key`; the public half is in
`tauri.conf.json`, the private half is the `TAURI_SIGNING_PRIVATE_KEY` secret).
`createUpdaterArtifacts` makes the build emit a `.app.tar.gz` and a `.sig`,
`publish-r2.mjs` fills in `platforms`, and the banner's **Install and restart**
calls the `install_update` command in `lib.rs`. That command lives in Rust
because the frontend reaches the shell through `window.__TAURI__` and carries no
Tauri npm packages.

The signature is the load-bearing part: the artefact is verified against the
public key before a byte is written, so replacing a running application cannot be
turned into a way to install something else. Break the signing and the button
does not silently degrade to a download — it refuses.

**The server** uses `wired update`, which fetches the published binaries,
verifies them by running `wired --version` against what the manifest promised,
swaps them with a `rename` in the install directory, and restarts the unit. Old
binaries move aside rather than away, so a failure between the two renames is put
back instead of leaving a mismatched pair. It never needs a compiler, and it does
not need root unless the install directory does.

What it will not do is update across a signing boundary it cannot check: with no
`server` entry for the platform it falls back to re-running the installer from a
checkout, and failing that it prints the download.

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
