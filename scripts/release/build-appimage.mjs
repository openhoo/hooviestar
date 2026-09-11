import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, readFileSync, rmSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { patchAndRepackAppImage } from "./patch-appimage-bootstrap.mjs";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..");

function fail(message) {
  throw new Error(`build-appimage: ${message}`);
}

function targetFromArgs(args) {
  const buildArgs = args[0] === "build" ? args.slice(1) : args;
  const index = buildArgs.findIndex((argument) => argument === "--target");
  if (index >= 0) {
    const target = buildArgs[index + 1];
    if (!target) {
      fail("--target requires a target triple");
    }
    return target;
  }
  const inline = buildArgs.find((argument) => argument.startsWith("--target="));
  return inline?.slice("--target=".length) || process.env.TARGET;
}

function releaseBuildArgs(args) {
  const buildArgs = args[0] === "build" ? args.slice(1) : args;
  if (buildArgs.includes("--debug")) {
    fail("tauri-appimage only supports release bundles; remove --debug");
  }
  const profileIndex = buildArgs.findIndex((argument) => argument === "--profile");
  const profileInline = buildArgs.find((argument) => argument.startsWith("--profile="));
  if (profileIndex >= 0 && !buildArgs[profileIndex + 1]) {
    fail("--profile requires a profile name");
  }
  const profile = profileInline?.slice("--profile=".length) || (profileIndex >= 0 ? buildArgs[profileIndex + 1] : undefined);
  if (profile && profile !== "release") {
    fail(`tauri-appimage only supports the release profile, not ${profile}`);
  }
  if (buildArgs.includes("--target-dir") || buildArgs.some((argument) => argument.startsWith("--target-dir="))) {
    fail("set CARGO_TARGET_DIR instead of passing --target-dir so the output path is unambiguous");
  }
  if (buildArgs.includes("--no-bundle")) {
    fail("tauri-appimage requires bundling an AppImage");
  }
  const bundlesIndex = buildArgs.findIndex((argument) => argument === "--bundles");
  const bundlesInline = buildArgs.find((argument) => argument.startsWith("--bundles="));
  const bundles = bundlesInline?.slice("--bundles=".length) || (bundlesIndex >= 0 ? buildArgs[bundlesIndex + 1] : undefined);
  if (bundles && !bundles.split(",").includes("appimage")) {
    fail(`tauri-appimage requires an appimage bundle, not ${bundles}`);
  }
  return buildArgs;
}

function cargoTargetDirectory() {
  try {
    const metadata = JSON.parse(
      execFileSync("cargo", ["metadata", "--no-deps", "--format-version", "1"], {
        cwd: repoRoot,
        encoding: "utf8",
      }),
    );
    if (typeof metadata.target_directory !== "string") {
      fail("cargo metadata did not report target_directory");
    }
    return resolve(metadata.target_directory);
  } catch (error) {
    if (error instanceof Error && error.message.startsWith("build-appimage:")) {
      throw error;
    }
    throw new Error(`build-appimage: cargo metadata failed: ${error.message}`);
  }
}

function runTauriBuild(args) {
  const npm = process.platform === "win32" ? "npm.cmd" : "npm";
  const buildArgs = args[0] === "build" ? args.slice(1) : args;
  const result = spawnSync(npm, ["run", "tauri", "--", "build", ...buildArgs], {
    cwd: repoRoot,
    env: process.env,
    stdio: "inherit",
  });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}

function configPaths(args) {
  const paths = [];
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === "--config") {
      if (!args[index + 1]) {
        fail("--config requires a path or JSON object");
      }
      paths.push(args[index + 1]);
      index += 1;
    } else if (argument.startsWith("--config=")) {
      paths.push(argument.slice("--config=".length));
    }
  }
  return paths;
}

function readConfig(value) {
  try {
    if (value.trimStart().startsWith("{")) {
      return JSON.parse(value);
    }
    return JSON.parse(readFileSync(resolve(repoRoot, value), "utf8"));
  } catch (error) {
    throw new Error(`build-appimage: cannot read Tauri config ${value}: ${error.message}`);
  }
}

function updaterArtifactsEnabled(args) {
  const baseConfig = JSON.parse(readFileSync(join(repoRoot, "src-tauri/tauri.conf.json"), "utf8"));
  let enabled = baseConfig.bundle?.createUpdaterArtifacts !== false;
  for (const value of configPaths(args)) {
    const config = readConfig(value);
    if (typeof config.bundle?.createUpdaterArtifacts === "boolean") {
      enabled = config.bundle.createUpdaterArtifacts;
    }
  }
  return enabled;
}

function effectiveAppImageIdentity(args, target) {
  const baseConfig = JSON.parse(readFileSync(join(repoRoot, "src-tauri/tauri.conf.json"), "utf8"));
  let productName = baseConfig.productName;
  let version = baseConfig.version;
  for (const value of configPaths(args)) {
    const config = readConfig(value);
    if (typeof config.productName === "string") {
      productName = config.productName;
    }
    if (typeof config.version === "string") {
      version = config.version;
    }
  }
  if (typeof productName !== "string" || typeof version !== "string") {
    fail("effective Tauri productName/version is missing");
  }
  const architecture = target?.split("-", 1)[0] || process.arch;
  const appImageArchitecture =
    architecture === "x64" || architecture === "x86_64"
      ? "amd64"
      : architecture === "ia32" || architecture === "i686"
        ? "i386"
        : architecture === "aarch64"
          ? "aarch64"
          : architecture === "armv7"
            ? "armhf"
            : architecture;
  return {
    appImage: `${productName}_${version}_${appImageArchitecture}.AppImage`,
    appDir: `${productName}.AppDir`,
  };
}

function signIfNeeded(appImage, shouldSign) {
  if (!shouldSign) {
    return;
  }
  const signature = `${appImage}.sig`;
  rmSync(signature, { force: true });
  const npm = process.platform === "win32" ? "npm.cmd" : "npm";
  const result = spawnSync(npm, ["run", "tauri", "--", "signer", "sign", appImage], {
    cwd: repoRoot,
    env: process.env,
    stdio: "inherit",
  });
  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}

function main() {
  const args = process.argv.slice(2);
  const buildArgs = releaseBuildArgs(args);
  const target = targetFromArgs(args);
  const targetDirectory = cargoTargetDirectory();
  const identity = effectiveAppImageIdentity(args, target);
  const appImageBundleDirectory = target
    ? join(targetDirectory, target, "release", "bundle", "appimage")
    : join(targetDirectory, "release", "bundle", "appimage");

  runTauriBuild(buildArgs);
  const appImage = join(appImageBundleDirectory, identity.appImage);
  const appDir = join(appImageBundleDirectory, identity.appDir);
  if (!existsSync(appImage)) {
    fail(`expected produced AppImage is missing: ${appImage}`);
  }
  if (!existsSync(appDir)) {
    fail(`expected produced AppDir is missing: ${appDir}`);
  }
  const shouldSign = updaterArtifactsEnabled(args);
  rmSync(`${appImage}.sig`, { force: true });
  patchAndRepackAppImage(appDir, appImage);
  signIfNeeded(appImage, shouldSign);
  console.log(`Built and bootstrapped ${appImage}`);
}

main();
