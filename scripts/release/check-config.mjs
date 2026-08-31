import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { isReleaseSemver } from "./semver.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const read = (path) => readFileSync(resolve(root, path), "utf8");
const readBuffer = (path) => readFileSync(resolve(root, path));
const readJson = (path) => JSON.parse(read(path));
const fail = (message) => {
  throw new Error(`release configuration: ${message}`);
};
const expect = (condition, message) => {
  if (!condition) fail(message);
};

const packageJson = readJson("package.json");
const packageLock = readJson("package-lock.json");
const tauri = readJson("src-tauri/tauri.conf.json");
const localBuildTauri = readJson("src-tauri/tauri.local-build.conf.json");
const cargoToml = read("Cargo.toml");
const tauriCargoToml = read("src-tauri/Cargo.toml");
const cargoLock = read("Cargo.lock");
const readme = read("README.md");
const releaseWorkflow = read(".github/workflows/release.yml");
const ciWorkflow = read(".github/workflows/ci.yml");
const tauriRuntime = read("src-tauri/src/lib.rs");
const updaterRuntime = read("src-tauri/src/updater.rs");
const taskbarRuntime = read("src-tauri/src/taskbar.rs");
const frontend = read("src/App.tsx");

const workspaceVersion = cargoToml.match(
  /\[workspace\.package\][\s\S]*?^version\s*=\s*"([^"]+)"/m,
)?.[1];
expect(workspaceVersion, "workspace package version is missing");
const version = packageJson.version;
expect(isReleaseSemver(version), "project version is not strict SemVer");
expect(version === workspaceVersion, "package.json and Cargo workspace versions differ");
expect(tauri.version === version, "tauri.conf.json version differs");
expect(packageLock.version === version, "package-lock.json root version differs");
expect(packageLock.packages?.[""]?.version === version, "package-lock root package version differs");
expect(
  readme.includes(`Hooviestar is at version ${version} and under active development.`),
  "README status version differs",
);

const workspaceLockVersions = cargoLock
  .split("\n[[package]]\n")
  .filter((section) => /^name = "hooviestar(?:-engine)?"$/m.test(section))
  .map((section) => section.match(/^version = "([^"]+)"$/m)?.[1]);
expect(workspaceLockVersions.length === 2, "Cargo.lock lacks workspace packages");
expect(
  workspaceLockVersions.every((entry) => entry === version),
  "Cargo.lock workspace versions differ",
);

expect(tauri.bundle?.createUpdaterArtifacts === true, "updater artifacts are disabled");
expect(
  localBuildTauri.bundle?.createUpdaterArtifacts === false,
  "local unsigned-build updater override is missing",
);
for (const target of ["nsis", "appimage", "deb"]) {
  expect(tauri.bundle?.targets?.includes(target), `${target} installer target is missing`);
}
expect(tauri.bundle?.publisher === "OpenHoo", "bundle publisher is missing");
expect(
  tauri.bundle?.homepage === "https://github.com/openhoo/hooviestar",
  "bundle homepage is missing",
);
expect(tauri.bundle?.license === "MIT", "bundle license is missing");
expect(tauri.bundle?.category === "Video", "bundle category is missing");
expect(tauri.bundle?.windows?.allowDowngrades === false, "Windows downgrade protection is disabled");
expect(
  tauri.bundle?.windows?.webviewInstallMode?.type === "embedBootstrapper",
  "embedded WebView2 bootstrapper is missing",
);
expect(
  ["English", "German"].every((language) =>
    tauri.bundle?.windows?.nsis?.languages?.includes(language)
  ),
  "English and German NSIS languages are required",
);
expect(tauri.app?.windows?.[0]?.theme === "Dark", "Studio native theme is not dark");
expect(
  tauri.app?.windows?.[0]?.backgroundColor === "#0b0d10",
  "Studio startup background does not match the UI",
);

const pngDimensions = (path) => {
  const png = readBuffer(path);
  expect(png.subarray(0, 8).equals(Buffer.from("89504e470d0a1a0a", "hex")), `${path} is not PNG`);
  return { width: png.readUInt32BE(16), height: png.readUInt32BE(20) };
};
const iconPng = pngDimensions("src-tauri/icons/icon.png");
expect(iconPng.width === 512 && iconPng.height === 512, "bundle PNG icon must be 512x512");
const uiIcon = pngDimensions("src/assets/hooviestar-icon-64.png");
expect(uiIcon.width === 64 && uiIcon.height === 64, "Studio brand icon must be 64x64");
const iconSource = pngDimensions("assets/branding/hooviestar-icon-source.png");
expect(
  iconSource.width === iconSource.height && iconSource.width >= 1024,
  "branding source must be square and at least 1024px",
);
const ico = readBuffer("src-tauri/icons/icon.ico");
expect(ico.readUInt16LE(0) === 0 && ico.readUInt16LE(2) === 1, "bundle ICO header is invalid");
const icoCount = ico.readUInt16LE(4);
const icoSizes = Array.from({ length: icoCount }, (_, index) => {
  const width = ico[6 + index * 16];
  const height = ico[7 + index * 16];
  const normalizedWidth = width === 0 ? 256 : width;
  const normalizedHeight = height === 0 ? 256 : height;
  return `${normalizedWidth}x${normalizedHeight}`;
}).sort((left, right) => Number(left.split("x")[0]) - Number(right.split("x")[0]));
expect(
  JSON.stringify(icoSizes) ===
    JSON.stringify(["16x16", "24x24", "32x32", "48x48", "64x64", "256x256"]),
  "bundle ICO must contain 16, 24, 32, 48, 64 and 256px frames",
);
const updater = tauri.plugins?.updater;
expect(
  updater?.endpoints?.includes(
    "https://github.com/openhoo/hooviestar/releases/latest/download/latest.json",
  ),
  "GitHub latest.json updater endpoint is missing",
);
expect(typeof updater?.pubkey === "string" && updater.pubkey.length > 80, "updater public key is missing");
const decodedKey = Buffer.from(updater.pubkey, "base64").toString("utf8");
expect(decodedKey.startsWith("untrusted comment: minisign public key:"), "updater public key is invalid");
expect(tauriRuntime.includes("tauri_plugin_updater::Builder"), "updater plugin is not registered");
const singleInstancePlugin = tauriRuntime.indexOf(".plugin(tauri_plugin_single_instance::init");
expect(singleInstancePlugin >= 0, "single-instance plugin is not registered");
expect(
  singleInstancePlugin === tauriRuntime.indexOf(".plugin("),
  "single-instance must remain the first registered plugin",
);
expect(
  tauriCargoToml.includes('tauri-plugin-single-instance = "2.4.4"'),
  "single-instance dependency is missing or unexpectedly changed",
);
expect(
  tauriRuntime.includes('with_filter(|label| label == "studio")'),
  "window-state persistence is not restricted to Studio",
);
for (const flag of ["POSITION", "SIZE", "MAXIMIZED"]) {
  expect(tauriRuntime.includes(`StateFlags::${flag}`), `window-state ${flag} flag is missing`);
}
for (const unsafeFlag of ["VISIBLE", "FULLSCREEN"]) {
  expect(
    !tauriRuntime.includes(`StateFlags::${unsafeFlag}`),
    `window-state must not restore ${unsafeFlag}`,
  );
}
expect(
  tauriCargoToml.includes('tauri-plugin-window-state = "2.4.1"'),
  "window-state dependency is missing or unexpectedly changed",
);
expect(
  ["studio.show()", "studio.unminimize()", "studio.set_focus()"].every((call) =>
    tauriRuntime.includes(call)
  ),
  "second launch does not fully restore and focus Studio",
);
expect(tauriRuntime.includes("updater::spawn"), "automatic updater is not started");
expect(updaterRuntime.includes("download_and_install"), "automatic update installation is missing");
expect(updaterRuntime.includes("download_percentage"), "updater download progress is missing");
expect(updaterRuntime.includes("UpdateStatus::Installing"), "updater install phase is missing");
expect(taskbarRuntime.includes("set_progress_bar"), "native taskbar progress is missing");
expect(taskbarRuntime.includes("set_overlay_icon"), "Windows taskbar error overlay is missing");
expect(
  taskbarRuntime.includes("self.device == Activity::Error || self.update == Activity::Error"),
  "taskbar error-priority guard is missing",
);
expect(updaterRuntime.includes("CHECK_TIMEOUT"), "automatic update check timeout is missing");
expect(updaterRuntime.includes("DOWNLOAD_TIMEOUT"), "automatic update download timeout is missing");
expect(updaterRuntime.includes("app.restart()"), "automatic restart after update is missing");
expect(frontend.includes('listen<UpdateStatusEvent>("updater-status"'), "updater status UI is missing");

expect(releaseWorkflow.includes("TAURI_SIGNING_PRIVATE_KEY"), "updater signing secret is not wired");
expect(releaseWorkflow.includes("release:verify-updater-key"), "updater key-pair proof is missing");
expect(releaseWorkflow.includes("npm audit --audit-level=high"), "release npm advisory gate is missing");
expect(releaseWorkflow.includes("WINDOWS_CERTIFICATE"), "Windows signing certificate is not wired");
expect(
  releaseWorkflow.includes("WINDOWS_CERTIFICATE_TRUST_MODE"),
  "Windows certificate trust mode is not explicit",
);
expect(
  releaseWorkflow.includes("Self-signed trust mode requires a self-signed certificate"),
  "self-signed Windows certificate guard is missing",
);
expect(
  releaseWorkflow.includes("$_.ObjectId -eq $codeSigningOid") &&
    releaseWorkflow.includes("$_.Value -eq $codeSigningOid"),
  "Windows code-signing EKU compatibility check is missing",
);
expect(
  releaseWorkflow.includes('$signature.Status -ne "UnknownError"') &&
    releaseWorkflow.includes(
      'terminated in a root certificate which is not trusted by the trust provider',
    ),
  "self-signed untrusted-root verification is missing",
);
expect(releaseWorkflow.includes("finally {"), "temporary PFX cleanup is not fail-safe");
expect(releaseWorkflow.includes("$cert.NotBefore"), "Windows certificate start-date validation is missing");
expect(releaseWorkflow.includes("actions/workflows/ci.yml/runs"), "successful tagged-SHA CI gate is missing");
expect(releaseWorkflow.includes("assert-newer-version.mjs"), "stable release ordering gate is missing");
expect(releaseWorkflow.includes("gh release verify"), "immutable release verification is missing");
expect(releaseWorkflow.includes("Get-AuthenticodeSignature"), "Authenticode verification is missing");
expect(releaseWorkflow.includes("TimeStamperCertificate"), "Authenticode timestamp verification is missing");
expect(releaseWorkflow.includes("7z t $installers[0].FullName"), "NSIS archive verification is missing");
expect(releaseWorkflow.includes("--appimage-extract"), "AppImage structure verification is missing");
expect(releaseWorkflow.includes("dpkg-deb --field"), "Debian package verification is missing");
expect(releaseWorkflow.includes("releaseDraft: true"), "release is not staged as a draft");
expect(releaseWorkflow.includes("--draft=false"), "verified draft publication is missing");
expect(releaseWorkflow.includes("latest.json"), "updater manifest verification is missing");
const publishDraftPosition = releaseWorkflow.indexOf('gh release edit "$GITHUB_REF_NAME"');
const liveManifestPosition = releaseWorkflow.indexOf(
  'release_api=$(gh api "repos/$GITHUB_REPOSITORY/releases/tags/$GITHUB_REF_NAME")',
);
expect(
  publishDraftPosition >= 0 && liveManifestPosition > publishDraftPosition,
  "live updater URL verification must run after draft publication",
);
expect(releaseWorkflow.includes("uploadUpdaterJson: false"), "matrix updater-manifest writes are not disabled");
expect(
  releaseWorkflow.includes("build-updater-manifest.mjs"),
  "single-writer updater manifest finalization is missing",
);
expect(
  releaseWorkflow.includes('minisign -Vm "$installer"'),
  "published updater artifact signature verification is missing",
);
expect(releaseWorkflow.includes('npm run tauri -- signer sign "$deb"'), "Debian updater signing is missing");
expect(releaseWorkflow.includes('"linux-x86_64-deb"'), "Debian updater manifest entry is missing");
expect(releaseWorkflow.includes("SHA256SUMS"), "release checksums are missing");
expect(releaseWorkflow.includes("anchore/sbom-action@"), "SPDX SBOM generation is missing");
expect(releaseWorkflow.includes("https://spdx.dev/Document/v2.3"), "SBOM attestation verification is missing");
expect(releaseWorkflow.includes("actions/attest@"), "release attestations are missing");
expect(
  releaseWorkflow.includes('--signer-workflow "$GITHUB_REPOSITORY/.github/workflows/release.yml"'),
  "attestation signer-workflow policy is missing",
);
expect(releaseWorkflow.includes('--source-digest "$GITHUB_SHA"'), "attestation source-SHA policy is missing");
expect(releaseWorkflow.includes("gh release verify-asset"), "immutable release attestation readback is missing");
expect(releaseWorkflow.includes("git ls-remote origin"), "remote release-tag commit readback is missing");
expect(releaseWorkflow.includes("cosign sign-blob"), "Sigstore checksum signing is missing");
expect(releaseWorkflow.includes("cosign verify-blob"), "Sigstore checksum verification is missing");
expect(
  releaseWorkflow.includes("--certificate-github-workflow-sha"),
  "Sigstore workflow identity verification is missing",
);

const workflowSources = [releaseWorkflow, ciWorkflow].join("\n");
const mutableActions = [...workflowSources.matchAll(/uses:\s*([^\s#]+)@([^\s#]+)/g)].filter(
  ([, , revision]) => !/^[0-9a-f]{40}$/.test(revision),
);
expect(
  mutableActions.length === 0,
  `CI and release actions must use immutable SHAs: ${mutableActions.map(([, action, revision]) => `${action}@${revision}`).join(", ")}`,
);
expect(!/^\s{2}bundle:\s*$/m.test(ciWorkflow), "ordinary CI still runs costly installer bundles");
expect(!/^\s+toolchain:\s*stable\s*$/m.test(workflowSources), "workflow Rust toolchain is moving");
expect(!/^\s+node-version:\s*24\s*$/m.test(workflowSources), "workflow Node version is moving");
expect(releaseWorkflow.includes("toolchain: 1.98.0"), "release Rust toolchain is not pinned");
expect(releaseWorkflow.includes("node-version: 24.20.0"), "release Node version is not pinned");

const tag = process.env.GITHUB_REF_NAME;
if (process.env.GITHUB_REF_TYPE === "tag") {
  expect(tag, "tag ref name is missing");
  expect(tag === `v${version}`, `tag ${tag} does not match version v${version}`);
}

console.log(`release configuration valid for v${version}`);
