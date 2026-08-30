import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { basename, dirname, join, resolve } from "node:path";
import { isReleaseSemver } from "./semver.mjs";

const modulePath = fileURLToPath(import.meta.url);
const root = resolve(dirname(modulePath), "../..");

function requireExactlyOne(assets, predicate, description) {
  const matches = assets.filter(predicate);
  if (matches.length !== 1) {
    throw new Error(`expected exactly one ${description}, found ${matches.length}`);
  }
  return matches[0];
}

function readUpdaterSignature(assetsDir, name) {
  const encoded = readFileSync(join(assetsDir, name), "utf8").trim();
  const decoded = Buffer.from(encoded, "base64").toString("utf8");
  if (!decoded.startsWith("untrusted comment: signature from tauri secret key")) {
    throw new Error(`${name} is not a Tauri updater signature`);
  }
  return encoded;
}

export function buildUpdaterManifest({ release, assetsDir, repository, version }) {
  if (release.tag_name !== `v${version}`) {
    throw new Error(`release tag ${release.tag_name} does not match v${version}`);
  }
  if (!release.draft) {
    throw new Error("updater manifest must be finalized while release is still a draft");
  }
  if (!Array.isArray(release.assets)) {
    throw new Error("release assets are missing");
  }

  const windows = requireExactlyOne(
    release.assets,
    (asset) => asset.name.endsWith(".exe"),
    "Windows NSIS updater asset",
  );
  const linux = requireExactlyOne(
    release.assets,
    (asset) => asset.name.endsWith(".AppImage"),
    "Linux AppImage updater asset",
  );
  const debian = requireExactlyOne(
    release.assets,
    (asset) => asset.name.endsWith(".deb"),
    "Debian updater asset",
  );
  const windowsSignature = requireExactlyOne(
    release.assets,
    (asset) => asset.name === `${windows.name}.sig`,
    "Windows updater signature",
  );
  const linuxSignature = requireExactlyOne(
    release.assets,
    (asset) => asset.name === `${linux.name}.sig`,
    "Linux updater signature",
  );
  const debianSignature = requireExactlyOne(
    release.assets,
    (asset) => asset.name === `${debian.name}.sig`,
    "Debian updater signature",
  );

  for (const asset of [
    windows,
    linux,
    debian,
    windowsSignature,
    linuxSignature,
    debianSignature,
  ]) {
    const localName = basename(asset.name);
    if (localName !== asset.name) {
      throw new Error(`unsafe release asset name: ${asset.name}`);
    }
    if (!Number.isSafeInteger(asset.id) || asset.id <= 0) {
      throw new Error(`invalid release asset id for ${asset.name}`);
    }
    readFileSync(join(assetsDir, localName));
  }

  const apiAssetUrl = (asset) =>
    `https://api.github.com/repos/${repository}/releases/assets/${asset.id}`;
  const windowsEntry = {
    signature: readUpdaterSignature(assetsDir, windowsSignature.name),
    url: apiAssetUrl(windows),
  };
  const linuxEntry = {
    signature: readUpdaterSignature(assetsDir, linuxSignature.name),
    url: apiAssetUrl(linux),
  };
  const debianEntry = {
    signature: readUpdaterSignature(assetsDir, debianSignature.name),
    url: apiAssetUrl(debian),
  };

  return {
    version,
    notes: typeof release.body === "string" ? release.body : "",
    pub_date: release.created_at,
    platforms: {
      "windows-x86_64": windowsEntry,
      "windows-x86_64-nsis": windowsEntry,
      "linux-x86_64": linuxEntry,
      "linux-x86_64-appimage": linuxEntry,
      "linux-x86_64-deb": debianEntry,
    },
  };
}

async function main() {
  const assetsDir = resolve(process.argv[2] ?? "release-assets");
  const repository = process.env.GITHUB_REPOSITORY;
  const tag = process.env.GITHUB_REF_NAME;
  const token = process.env.GH_TOKEN;
  if (!repository || !tag || !token) {
    throw new Error("GITHUB_REPOSITORY, GITHUB_REF_NAME, and GH_TOKEN are required");
  }
  if (!tag.startsWith("v") || !isReleaseSemver(tag.slice(1))) {
    throw new Error(`invalid release tag: ${tag}`);
  }

  const packageVersion = JSON.parse(readFileSync(join(root, "package.json"), "utf8")).version;
  if (tag !== `v${packageVersion}`) {
    throw new Error(`release tag ${tag} does not match package version ${packageVersion}`);
  }
  const response = await fetch(
    `https://api.github.com/repos/${repository}/releases/tags/${encodeURIComponent(tag)}`,
    {
      headers: {
        Accept: "application/vnd.github+json",
        Authorization: `Bearer ${token}`,
        "X-GitHub-Api-Version": "2022-11-28",
      },
    },
  );
  if (!response.ok) {
    throw new Error(`failed to read draft release: HTTP ${response.status}`);
  }
  const release = await response.json();
  const manifest = buildUpdaterManifest({
    release,
    assetsDir,
    repository,
    version: packageVersion,
  });
  writeFileSync(join(assetsDir, "latest.json"), `${JSON.stringify(manifest, null, 2)}\n`, {
    mode: 0o600,
  });
  console.log(`built updater manifest for ${tag}`);
}

if (process.argv[1] && resolve(process.argv[1]) === modulePath) {
  await main();
}
