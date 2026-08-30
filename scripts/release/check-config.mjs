import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { isReleaseSemver } from "./semver.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const read = (path) => readFileSync(resolve(root, path), "utf8");
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
const cargoLock = read("Cargo.lock");
const readme = read("README.md");
const releaseWorkflow = read(".github/workflows/release.yml");
const ciWorkflow = read(".github/workflows/ci.yml");
const tauriRuntime = read("src-tauri/src/lib.rs");
const updaterRuntime = read("src-tauri/src/updater.rs");
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
expect(tauriRuntime.includes("updater::spawn"), "automatic updater is not started");
expect(updaterRuntime.includes("download_and_install"), "automatic update installation is missing");
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
