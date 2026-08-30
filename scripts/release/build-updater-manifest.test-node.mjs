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
    release: {
      tag_name: "v0.2.0",
      draft: true,
      body: "Release notes",
      created_at: "2026-08-30T20:00:00Z",
      assets: assets.map(({ id, name }) => ({ id, name })),
    },
  };
}

test("builds complete NSIS, AppImage, and Debian updater entries from finalized assets", () => {
  const data = fixture();
  try {
    const manifest = buildUpdaterManifest({
      ...data,
      repository: "openhoo/hooviestar",
      version: "0.2.0",
    });
    assert.equal(manifest.version, "0.2.0");
    assert.equal(manifest.notes, "Release notes");
    assert.deepEqual(Object.keys(manifest.platforms), [
      "windows-x86_64",
      "windows-x86_64-nsis",
      "linux-x86_64",
      "linux-x86_64-appimage",
      "linux-x86_64-deb",
    ]);
    assert.match(manifest.platforms["windows-x86_64"].url, /assets\/101$/);
    assert.match(manifest.platforms["linux-x86_64"].url, /assets\/103$/);
    assert.match(manifest.platforms["linux-x86_64-deb"].url, /assets\/105$/);
    assert.equal(manifest.platforms["windows-x86_64"].signature, signature);
  } finally {
    rmSync(data.assetsDir, { recursive: true, force: true });
  }
});

test("rejects a partial updater asset set", () => {
  const data = fixture();
  try {
    data.release.assets = data.release.assets.filter((asset) => !asset.name.endsWith(".deb.sig"));
    assert.throws(
      () =>
        buildUpdaterManifest({
          ...data,
          repository: "openhoo/hooviestar",
          version: "0.2.0",
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
    data.release.assets.push({ id: 107, name: "Hooviestar_0.2.0_portable.exe" });
    writeFileSync(join(data.assetsDir, "Hooviestar_0.2.0_portable.exe"), "exe");
    assert.throws(
      () =>
        buildUpdaterManifest({
          ...data,
          repository: "openhoo/hooviestar",
          version: "0.2.0",
        }),
      /exactly one Windows NSIS updater asset, found 2/,
    );
  } finally {
    rmSync(data.assetsDir, { recursive: true, force: true });
  }
});

test("rejects an invalid release asset id", () => {
  const data = fixture();
  try {
    data.release.assets.find((asset) => asset.name.endsWith(".AppImage")).id = 0;
    assert.throws(
      () =>
        buildUpdaterManifest({
          ...data,
          repository: "openhoo/hooviestar",
          version: "0.2.0",
        }),
      /invalid release asset id/,
    );
  } finally {
    rmSync(data.assetsDir, { recursive: true, force: true });
  }
});
