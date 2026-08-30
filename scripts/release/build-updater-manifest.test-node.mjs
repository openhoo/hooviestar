import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import assert from "node:assert/strict";
import { buildUpdaterManifest } from "./build-updater-manifest.mjs";

const signature = Buffer.from(
  "untrusted comment: signature from tauri secret key\nRUQfake\ntrusted comment: test\nRUQfake\n",
).toString("base64");

function fixture() {
  const assetsDir = mkdtempSync(join(tmpdir(), "hooviestar-manifest-test-"));
  const assets = [
    { id: 101, name: "Hooviestar_0.2.0_x64-setup.exe", content: "exe" },
    { id: 102, name: "Hooviestar_0.2.0_x64-setup.exe.sig", content: signature },
    { id: 103, name: "Hooviestar_0.2.0_amd64.AppImage", content: "appimage" },
    { id: 104, name: "Hooviestar_0.2.0_amd64.AppImage.sig", content: signature },
    { id: 105, name: "Hooviestar_0.2.0_amd64.deb", content: "deb" },
    { id: 106, name: "Hooviestar_0.2.0_amd64.deb.sig", content: signature },
  ];
  for (const asset of assets) writeFileSync(join(assetsDir, asset.name), asset.content);
  return {
    assetsDir,
  };
}

test("builds complete NSIS, AppImage, and Debian updater entries from finalized assets", () => {
  const data = fixture();
  try {
    const manifest = buildUpdaterManifest({
      ...data,
      repository: "openhoo/hooviestar",
      tag: "v0.2.0",
      version: "0.2.0",
      publishedAt: "2026-08-30T20:00:00Z",
    });
    assert.equal(manifest.version, "0.2.0");
    assert.equal(manifest.notes, "Hooviestar v0.2.0");
    assert.deepEqual(Object.keys(manifest.platforms), [
      "windows-x86_64",
      "windows-x86_64-nsis",
      "linux-x86_64",
      "linux-x86_64-appimage",
      "linux-x86_64-deb",
    ]);
    assert.equal(
      manifest.platforms["windows-x86_64"].url,
      "https://github.com/openhoo/hooviestar/releases/download/v0.2.0/Hooviestar_0.2.0_x64-setup.exe",
    );
    assert.match(manifest.platforms["linux-x86_64"].url, /Hooviestar_0\.2\.0_amd64\.AppImage$/);
    assert.match(manifest.platforms["linux-x86_64-deb"].url, /Hooviestar_0\.2\.0_amd64\.deb$/);
    assert.equal(manifest.platforms["windows-x86_64"].signature, signature);
  } finally {
    rmSync(data.assetsDir, { recursive: true, force: true });
  }
});

test("rejects a partial updater asset set", () => {
  const data = fixture();
  try {
    rmSync(join(data.assetsDir, "Hooviestar_0.2.0_amd64.deb.sig"));
    assert.throws(
      () =>
        buildUpdaterManifest({
          ...data,
          repository: "openhoo/hooviestar",
          tag: "v0.2.0",
          version: "0.2.0",
          publishedAt: "2026-08-30T20:00:00Z",
        }),
      /exactly one Debian updater signature/,
    );
  } finally {
    rmSync(data.assetsDir, { recursive: true, force: true });
  }
});

test("rejects a duplicated updater asset set", () => {
  const data = fixture();
  try {
    writeFileSync(join(data.assetsDir, "Hooviestar_0.2.0_portable.exe"), "exe");
    assert.throws(
      () =>
        buildUpdaterManifest({
          ...data,
          repository: "openhoo/hooviestar",
          tag: "v0.2.0",
          version: "0.2.0",
          publishedAt: "2026-08-30T20:00:00Z",
        }),
      /exactly one Windows NSIS updater asset, found 2/,
    );
  } finally {
    rmSync(data.assetsDir, { recursive: true, force: true });
  }
});

test("rejects an invalid repository name", () => {
  const data = fixture();
  try {
    assert.throws(
      () =>
        buildUpdaterManifest({
          ...data,
          repository: "openhoo/../hooviestar",
          tag: "v0.2.0",
          version: "0.2.0",
          publishedAt: "2026-08-30T20:00:00Z",
        }),
      /invalid GitHub repository name/,
    );
  } finally {
    rmSync(data.assetsDir, { recursive: true, force: true });
  }
});
