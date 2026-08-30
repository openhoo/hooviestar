import { readFileSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { isReleaseSemver } from "./semver.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const version = process.argv[2];
if (!isReleaseSemver(version)) {
  throw new Error("usage: npm run release:prepare -- <semver>");
}

const pathOf = (path) => resolve(root, path);
const read = (path) => readFileSync(pathOf(path), "utf8");
const writeJson = (path, value) => writeFileSync(pathOf(path), `${JSON.stringify(value, null, 2)}\n`);

const packageJson = JSON.parse(read("package.json"));
const packageLock = JSON.parse(read("package-lock.json"));
const originalFiles = new Map(
  [
    "package.json",
    "package-lock.json",
    "src-tauri/tauri.conf.json",
    "Cargo.toml",
    "Cargo.lock",
    "README.md",
  ].map((path) => [path, read(path)]),
);
let tauriConfig = originalFiles.get("src-tauri/tauri.conf.json");
let cargoToml = originalFiles.get("Cargo.toml");
let readme = originalFiles.get("README.md");
const tauriVersionPattern = /(^  "version"\s*:\s*")[^"]+("\s*,\s*$)/m;
const cargoVersionPattern = /(\[workspace\.package\][\s\S]*?^version\s*=\s*")[^"]+(".*$)/m;
const readmeVersionPattern = /(Hooviestar is at version )\S+( and under active development\.)/;

if (!tauriVersionPattern.test(tauriConfig)) {
  throw new Error("tauri.conf.json version field is missing");
}
if (!cargoVersionPattern.test(cargoToml)) {
  throw new Error("Cargo workspace version field is missing");
}
if (!readmeVersionPattern.test(readme)) {
  throw new Error("README status version is missing");
}

packageJson.version = version;
packageLock.version = version;
packageLock.packages[""].version = version;
tauriConfig = tauriConfig.replace(tauriVersionPattern, `$1${version}$2`);
cargoToml = cargoToml.replace(cargoVersionPattern, `$1${version}$2`);
readme = readme.replace(readmeVersionPattern, `$1${version}$2`);

let prepared = false;
try {
  writeJson("package.json", packageJson);
  writeJson("package-lock.json", packageLock);
  writeFileSync(pathOf("src-tauri/tauri.conf.json"), tauriConfig);
  writeFileSync(pathOf("Cargo.toml"), cargoToml);
  writeFileSync(pathOf("README.md"), readme);

  const lockUpdate = spawnSync(
    "cargo",
    ["update", "--offline", "-p", "hooviestar", "-p", "hooviestar-engine"],
    {
      cwd: root,
      stdio: ["ignore", "ignore", "inherit"],
    },
  );
  if (lockUpdate.error || lockUpdate.status !== 0) {
    throw new Error(`Cargo.lock update failed with exit code ${lockUpdate.status ?? "unknown"}`);
  }

  const check = spawnSync(process.execPath, [pathOf("scripts/release/check-config.mjs")], {
    cwd: root,
    stdio: "inherit",
  });
  if (check.error || check.status !== 0) {
    throw new Error(`release configuration check failed with exit code ${check.status ?? "unknown"}`);
  }
  prepared = true;
} finally {
  if (!prepared) {
    for (const [path, contents] of originalFiles) writeFileSync(pathOf(path), contents);
  }
}

console.log(`prepared Hooviestar v${version}; review, qualify, commit, then create annotated tag v${version}`);
