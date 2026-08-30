import { readFileSync, readdirSync, writeFileSync } from "node:fs";
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

export function buildUpdaterManifest({ assetsDir, repository, tag, version, publishedAt }) {
  if (tag !== `v${version}`) {
    throw new Error(`release tag ${tag} does not match v${version}`);
  }
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository)) {
    throw new Error("invalid GitHub repository name");
  }
  if (typeof publishedAt !== "string" || Number.isNaN(Date.parse(publishedAt))) {
    throw new Error("invalid updater publication date");
  }
  const assets = readdirSync(assetsDir, { withFileTypes: true })
    .filter((entry) => entry.isFile())
    .map((entry) => entry.name);

  const windows = requireExactlyOne(
    assets,
    (asset) => asset.endsWith(".exe"),
    "Windows NSIS updater asset",
  );
  const linux = requireExactlyOne(
    assets,
    (asset) => asset.endsWith(".AppImage"),
    "Linux AppImage updater asset",
  );
  const debian = requireExactlyOne(
    assets,
    (asset) => asset.endsWith(".deb"),
    "Debian updater asset",
  );
  const windowsSignature = requireExactlyOne(
    assets,
    (asset) => asset === `${windows}.sig`,
    "Windows updater signature",
  );
  const linuxSignature = requireExactlyOne(
    assets,
    (asset) => asset === `${linux}.sig`,
    "Linux updater signature",
  );
  const debianSignature = requireExactlyOne(
    assets,
    (asset) => asset === `${debian}.sig`,
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
    const localName = basename(asset);
    if (localName !== asset || !/^[A-Za-z0-9][A-Za-z0-9._+-]*$/.test(asset)) {
      throw new Error(`unsafe release asset name: ${asset}`);
    }
    readFileSync(join(assetsDir, localName));
  }

  const releaseDownloadUrl = (asset) =>
    `https://github.com/${repository}/releases/download/${encodeURIComponent(tag)}/${encodeURIComponent(asset)}`;
  const windowsEntry = {
    signature: readUpdaterSignature(assetsDir, windowsSignature),
    url: releaseDownloadUrl(windows),
  };
  const linuxEntry = {
    signature: readUpdaterSignature(assetsDir, linuxSignature),
    url: releaseDownloadUrl(linux),
  };
  const debianEntry = {
    signature: readUpdaterSignature(assetsDir, debianSignature),
    url: releaseDownloadUrl(debian),
  };

  return {
    version,
    notes: `Hooviestar ${tag}`,
    pub_date: publishedAt,
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
  if (!repository || !tag) {
    throw new Error("GITHUB_REPOSITORY and GITHUB_REF_NAME are required");
  }
  if (!tag.startsWith("v") || !isReleaseSemver(tag.slice(1))) {
    throw new Error(`invalid release tag: ${tag}`);
  }

  const packageVersion = JSON.parse(readFileSync(join(root, "package.json"), "utf8")).version;
  if (tag !== `v${packageVersion}`) {
    throw new Error(`release tag ${tag} does not match package version ${packageVersion}`);
  }
  const manifest = buildUpdaterManifest({
    assetsDir,
    repository,
    tag,
    version: packageVersion,
    publishedAt: new Date().toISOString(),
  });
  writeFileSync(join(assetsDir, "latest.json"), `${JSON.stringify(manifest, null, 2)}\n`, {
    mode: 0o600,
  });
  console.log(`built updater manifest for ${tag}`);
}

if (process.argv[1] && resolve(process.argv[1]) === modulePath) {
  await main();
}
