#!/usr/bin/env node
/**
 * Publish Tauri CI artifacts to Cloudflare R2 (wired-terminal-releases).
 *
 * Ported from wired-website, with two deliberate differences:
 *
 *  1. macOS is two builds, not a universal one. `release.yml` builds Apple
 *     silicon and Intel separately — the PTY layer links libc directly and the
 *     smaller download is worth two artefacts — so every key here is per
 *     target, and there is no `macos.zip` that means both.
 *
 *  2. Missing signatures are a warning, not a fatal error. The sibling dies
 *     with "No signed updater platforms to publish" because its updater is
 *     live. Here the updater plugin is not wired up yet (see
 *     docs/releasing.md), so `.sig` files do not exist — and refusing to
 *     publish would mean no downloads at all. Signed updater entries are added
 *     when the signatures appear; until then `platforms` is empty and only the
 *     `downloads` map is populated.
 *
 * Expects downloaded GitHub Actions artifacts under ARTIFACTS_DIR, one
 * directory per build target:
 *   artifacts/wired-terminal-aarch64-apple-darwin/...
 *   artifacts/wired-terminal-x86_64-pc-windows-msvc/...
 *
 * Auth (either works):
 *   A) Wrangler API token (Profile → API Tokens, Account R2 Edit):
 *        CLOUDFLARE_API_TOKEN + CLOUDFLARE_ACCOUNT_ID
 *   B) R2 S3 API token (R2 → Manage R2 API Tokens, Object Read & Write):
 *        R2_ACCESS_KEY_ID + R2_SECRET_ACCESS_KEY + CLOUDFLARE_ACCOUNT_ID
 *
 * Other env:
 *   R2_BUCKET              default: wired-terminal-releases
 *   RELEASES_PUBLIC_BASE   default: https://wired-terminal-releases.wired.dev
 *   ARTIFACTS_DIR          default: artifacts
 *   VERSION                optional override (else read from tauri.conf.json)
 *   RELEASE_NOTES          optional notes for latest.json
 *   DRY_RUN                if "1", print actions only
 */
import { execFileSync } from "node:child_process";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = join(__dirname, "../..");

const BUCKET = process.env.R2_BUCKET || "wired-terminal-releases";
const PUBLIC_BASE = (
  process.env.RELEASES_PUBLIC_BASE || "https://wired-terminal-releases.wired.dev"
).replace(/\/$/, "");
const ARTIFACTS_DIR = process.env.ARTIFACTS_DIR || join(REPO_ROOT, "artifacts");
const DRY_RUN = process.env.DRY_RUN === "1";
const STEM = "wired-terminal";

/**
 * One entry per build in the release matrix. `dir` matches the artifact
 * directory name so two near-identical macOS dmgs can never be confused for
 * each other — filename matching alone cannot tell them apart.
 */
const TARGETS = [
  {
    id: "macos-aarch64",
    label: "macOS (Apple silicon)",
    dir: "aarch64-apple-darwin",
    updaterKeys: ["darwin-aarch64"],
    updaterExt: ".app.tar.gz",
    installer: (f) => /\.dmg$/i.test(f),
    updaterAsset: (f) => /\.app\.tar\.gz$/i.test(f),
    updaterContentType: "application/gzip",
  },
  {
    id: "macos-x86_64",
    label: "macOS (Intel)",
    dir: "x86_64-apple-darwin",
    updaterKeys: ["darwin-x86_64"],
    updaterExt: ".app.tar.gz",
    installer: (f) => /\.dmg$/i.test(f),
    updaterAsset: (f) => /\.app\.tar\.gz$/i.test(f),
    updaterContentType: "application/gzip",
  },
  {
    id: "windows-x86_64",
    label: "Windows",
    dir: "x86_64-pc-windows-msvc",
    updaterKeys: ["windows-x86_64"],
    updaterExt: "-setup.exe",
    installer: (f) => /-setup\.exe$/i.test(f) || /\.msi$/i.test(f),
    updaterAsset: (f) => /-setup\.exe$/i.test(f),
    updaterContentType: "application/vnd.microsoft.portable-executable",
  },
  {
    id: "linux-x86_64",
    label: "Linux",
    dir: "x86_64-unknown-linux-gnu",
    updaterKeys: ["linux-x86_64"],
    updaterExt: ".AppImage",
    // Both, when both exist: an AppImage runs anywhere, a .deb is what an
    // Ubuntu user actually wants.
    installer: (f) => /\.AppImage$/i.test(f) || /\.deb$/i.test(f),
    // The AppImage itself, signed in place. `.AppImage.tar.gz` was Tauri v1's
    // shape and matched nothing here: v1.0.3 built a valid
    // `…AppImage.sig` and this script looked straight past it, so Linux was the
    // one platform published with downloads but no self-update.
    updaterAsset: (f) => /\.AppImage$/i.test(f),
    updaterContentType: "application/gzip",
    // The headless pair — `wired-backend` and `wired` — for a server install,
    // which wants neither a desktop bundle nor a Rust toolchain. Deliberately
    // narrower than the updater's `.AppImage.tar.gz`, so the two archives
    // published for this target cannot be mistaken for one another.
    serverAsset: (f) => /-server_.*\.tar\.gz$/i.test(base(f)),
  },
];

function die(msg) {
  console.error(msg);
  process.exit(1);
}

function walk(dir, out = []) {
  if (!existsSync(dir)) return out;
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    const st = statSync(p);
    if (st.isDirectory()) walk(p, out);
    else out.push(p);
  }
  return out;
}

const base = (f) => f.split(/[/\\]/).pop() || f;
const isSig = (f) => f.endsWith(".sig");

function readVersion() {
  if (process.env.VERSION) return process.env.VERSION.replace(/^v/, "");
  const confPath = join(REPO_ROOT, "app/src-tauri/tauri.conf.json");
  const conf = JSON.parse(readFileSync(confPath, "utf8"));
  return String(conf.version);
}

/**
 * Prefer Wrangler (CLOUDFLARE_API_TOKEN) when present — account-scoped R2
 * tokens work for every bucket. R2 S3 keys are often limited to one bucket,
 * which breaks a multi-product publish.
 */
function useS3() {
  if (process.env.CLOUDFLARE_API_TOKEN) return false;
  return Boolean(process.env.R2_ACCESS_KEY_ID && process.env.R2_SECRET_ACCESS_KEY);
}

function putObject(key, filePath, { contentType, contentDisposition, cacheControl }) {
  console.log(`→ r2://${BUCKET}/${key}  (${(statSync(filePath).size / 1e6).toFixed(2)} MB)`);
  if (DRY_RUN) {
    console.log(`  dry-run put ${key}`);
    return;
  }

  if (useS3()) {
    const accountId = process.env.CLOUDFLARE_ACCOUNT_ID;
    if (!accountId) die("CLOUDFLARE_ACCOUNT_ID is required for R2 S3 uploads");
    const args = [
      "s3",
      "cp",
      filePath,
      `s3://${BUCKET}/${key}`,
      "--endpoint-url",
      `https://${accountId}.r2.cloudflarestorage.com`,
    ];
    if (contentType) args.push("--content-type", contentType);
    if (contentDisposition) args.push("--content-disposition", contentDisposition);
    if (cacheControl) args.push("--cache-control", cacheControl);
    execFileSync("aws", args, {
      stdio: "inherit",
      env: {
        ...process.env,
        AWS_ACCESS_KEY_ID: process.env.R2_ACCESS_KEY_ID,
        AWS_SECRET_ACCESS_KEY: process.env.R2_SECRET_ACCESS_KEY,
        AWS_DEFAULT_REGION: "auto",
        AWS_EC2_METADATA_DISABLED: "true",
      },
    });
    return;
  }

  const args = [
    "wrangler",
    "r2",
    "object",
    "put",
    `${BUCKET}/${key}`,
    `--file=${filePath}`,
    "--remote",
  ];
  if (contentType) args.push("--content-type", contentType);
  if (contentDisposition) args.push("--content-disposition", contentDisposition);
  if (cacheControl) args.push("--cache-control", cacheControl);
  execFileSync("npx", args, { stdio: "inherit", env: process.env });
}

function zipFiles(outZip, paths) {
  mkdirSync(dirname(outZip), { recursive: true });
  console.log(`zip ${base(outZip)}`);
  if (DRY_RUN) {
    console.log("  dry-run zip", paths.map(base));
    return;
  }
  // -j flattens, -X drops extra file attributes so the zip is reproducible.
  execFileSync("zip", ["-j", "-X", outZip, ...paths], { stdio: "inherit" });
}

function main() {
  if (!DRY_RUN) {
    const hasApi = Boolean(process.env.CLOUDFLARE_API_TOKEN);
    const hasS3 = useS3();
    if (!hasApi && !hasS3) {
      console.log("Auth mode: wrangler OAuth / default credentials");
    } else {
      console.log("Auth mode:", hasS3 ? "R2 S3 API" : "Wrangler API token");
    }
    if (hasS3 && !process.env.CLOUDFLARE_ACCOUNT_ID) {
      die("CLOUDFLARE_ACCOUNT_ID is required for R2 S3 uploads");
    }
  }

  const version = readVersion();
  const notes = process.env.RELEASE_NOTES?.trim() || `Wired Terminal ${version}`;
  const pubDate = new Date().toISOString().replace(/\.\d{3}Z$/, "Z");

  console.log(`Publishing version ${version}`);
  console.log(`Artifacts dir: ${ARTIFACTS_DIR}`);

  const allFiles = walk(ARTIFACTS_DIR);
  if (allFiles.length === 0) die(`No files under ${ARTIFACTS_DIR}`);

  const staging = join(REPO_ROOT, ".publish-r2");
  mkdirSync(staging, { recursive: true });

  /** @type {Record<string, { signature: string, url: string }>} */
  const platforms = {};
  /** @type {Record<string, { url: string, label: string, filename: string }>} */
  const downloads = {};
  /** @type {Record<string, { url: string, alias: string, filename: string }>} */
  const server = {};

  for (const target of TARGETS) {
    // Scope to this target's artifact directory. Two macOS dmgs differ only by
    // the directory they came out of.
    const files = allFiles.filter((f) => f.includes(target.dir));
    if (files.length === 0) {
      console.log(`— ${target.label}: no artifacts (${target.dir})`);
      continue;
    }

    const installers = files.filter((f) => !isSig(f) && target.installer(f));
    if (installers.length === 0) {
      console.warn(`WARN: ${target.label}: no installer found in ${target.dir}`);
      continue;
    }
    console.log(`\n${target.label}: ${installers.map(base).join(", ")}`);

    // ── The download: one zip per target, versioned and aliased ──────────
    const stage = join(staging, target.id);
    mkdirSync(stage, { recursive: true });
    const staged = installers.map((f) => {
      const dest = join(stage, base(f));
      copyFileSync(f, dest);
      return dest;
    });

    const filename = `${STEM}_${version}_${target.id}.zip`;
    const verZip = join(staging, filename);
    zipFiles(verZip, staged);

    if (DRY_RUN) {
      // putObject would stat a zip that was never written; say what it would do.
      console.log(`  dry-run put downloads/${filename}`);
      console.log(`  dry-run put downloads/${target.id}.zip`);
    } else {
      putObject(`downloads/${filename}`, verZip, {
        contentType: "application/zip",
        contentDisposition: `attachment; filename="${filename}"`,
        cacheControl: "public, max-age=31536000, immutable",
      });

      // The versioned object is immutable; the stable alias must revalidate so
      // an old CDN entry never outlives a release. The site prefers versioned.
      const aliasZip = join(staging, `${target.id}.zip`);
      copyFileSync(verZip, aliasZip);
      putObject(`downloads/${target.id}.zip`, aliasZip, {
        contentType: "application/zip",
        contentDisposition: `attachment; filename="${filename}"`,
        cacheControl: "public, max-age=0, must-revalidate",
      });
    }

    downloads[target.id] = {
      label: target.label,
      filename,
      url: `${PUBLIC_BASE}/downloads/${filename}?v=${encodeURIComponent(version)}`,
    };

    // ── The headless server binaries, for targets that ship them ────────
    // Before the updater block on purpose: that one bails early when nothing is
    // signed, which is every release until the updater is wired up, and a
    // server tarball must not be collateral damage of that `continue`.
    if (target.serverAsset) {
      const serverAsset = files.find((f) => !isSig(f) && target.serverAsset(f));
      if (!serverAsset) {
        console.warn(`WARN: ${target.label}: no server tarball found in ${target.dir}`);
      } else {
        const serverName = `${STEM}-server_${version}_${target.id}.tar.gz`;
        const stagedServer = join(staging, serverName);
        copyFileSync(serverAsset, stagedServer);
        putObject(`downloads/${serverName}`, stagedServer, {
          contentType: "application/gzip",
          cacheControl: "public, max-age=31536000, immutable",
        });

        // The alias is the contract with install-ubuntu.sh: a fixed URL it can
        // `curl` with no manifest to parse and no `jq` to install first. It has
        // to revalidate, or a CDN copy outlives the release it came from.
        const aliasName = `${target.id}-server.tar.gz`;
        const stagedAlias = join(staging, aliasName);
        copyFileSync(serverAsset, stagedAlias);
        putObject(`downloads/${aliasName}`, stagedAlias, {
          contentType: "application/gzip",
          cacheControl: "public, max-age=0, must-revalidate",
        });

        server[target.id] = {
          filename: serverName,
          url: `${PUBLIC_BASE}/downloads/${serverName}?v=${encodeURIComponent(version)}`,
          alias: `${PUBLIC_BASE}/downloads/${aliasName}`,
        };
      }
    }

    // ── The updater entry, when it is signed ────────────────────────────
    const asset = files.find((f) => !isSig(f) && target.updaterAsset(f));
    const sig = asset
      ? files.find((f) => isSig(f) && base(f) === `${base(asset)}.sig`)
      : null;

    if (!asset) continue;
    if (!sig) {
      console.warn(
        `WARN: ${target.label}: ${base(asset)} has no .sig — skipping the updater entry ` +
          "(needs TAURI_SIGNING_PRIVATE_KEY and bundle.createUpdaterArtifacts)",
      );
      continue;
    }

    const key = `updates/${STEM}_${version}_${target.id}${target.updaterExt}`;
    const stagedAsset = join(staging, base(key));
    copyFileSync(asset, stagedAsset);
    putObject(key, stagedAsset, {
      contentType: target.updaterContentType,
      cacheControl: "public, max-age=31536000, immutable",
    });

    // ?v= cache-bust: a pre-publish edge 404 must not poison the updater for 4h.
    const url = `${PUBLIC_BASE}/${key}?v=${encodeURIComponent(version)}`;
    const signature = readFileSync(sig, "utf8").trim();
    for (const platformKey of target.updaterKeys) {
      platforms[platformKey] = { signature, url };
    }
  }

  if (Object.keys(downloads).length === 0) {
    die("Nothing to publish — no installer was found for any target");
  }

  if (Object.keys(platforms).length === 0) {
    console.warn(
      "\nWARN: publishing downloads with an empty `platforms` — nothing was signed, so " +
        "auto-update has nothing to offer. See docs/releasing.md.",
    );
  }

  // `platforms` is Tauri's updater contract. `downloads` is ours: it lets the
  // site link installers without reconstructing filenames, which is how a
  // rename here turns into a 404 there. `server` is the headless pair, kept
  // apart because a .deb and a systemd install are not the same download.
  const latest = { version, notes, pub_date: pubDate, platforms, downloads, server };
  const latestPath = join(staging, "latest.json");
  writeFileSync(latestPath, JSON.stringify(latest, null, 2) + "\n");
  console.log("\nlatest.json:\n", JSON.stringify(latest, null, 2));

  putObject("updates/latest.json", latestPath, {
    contentType: "application/json",
    cacheControl: "public, max-age=0, must-revalidate",
  });

  console.log("\nPublished to", PUBLIC_BASE);
  console.log("  Manifest:", `${PUBLIC_BASE}/updates/latest.json`);
  for (const [id, d] of Object.entries(downloads)) {
    console.log(`  ${d.label}:`, `${PUBLIC_BASE}/downloads/${d.filename}`);
    console.log(`  ${" ".repeat(d.label.length)} alias:`, `${PUBLIC_BASE}/downloads/${id}.zip`);
  }
  for (const [id, s] of Object.entries(server)) {
    console.log(`  server (${id}):`, s.alias);
  }
}

main();
