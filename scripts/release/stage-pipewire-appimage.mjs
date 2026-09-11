import { execFileSync } from "node:child_process";
import { cpSync, mkdirSync, readdirSync, rmSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

if (process.platform !== "linux") {
  process.exit(0);
}

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "../..");
const stageRoot = join(repoRoot, "src-tauri/resources/pipewire");

function pkgConfig(...args) {
  return execFileSync("pkg-config", args, { encoding: "utf8" }).trim();
}

function requirePath(path, kind, type) {
  const stat = statSync(path, { throwIfNoEntry: false });
  const valid = type === "directory" ? stat?.isDirectory() : stat?.isFile();
  if (!valid) {
    throw new Error(`required PipeWire ${kind} is missing: ${path}`);
  }
}

try {
  const libDir = pkgConfig("--variable=libdir", "libpipewire-0.3");
  const prefix = pkgConfig("--variable=prefix", "libpipewire-0.3");
  const moduleDir = pkgConfig("--variable=moduledir", "libpipewire-0.3");
  const spaDir = pkgConfig("--variable=plugindir", "libspa-0.2");
  const configDir = join(prefix, "share/pipewire");
  requirePath(moduleDir, "module directory", "directory");
  requirePath(spaDir, "SPA plugin directory", "directory");
  requirePath(configDir, "config directory", "directory");
  requirePath(join(spaDir, "support/libspa-support.so"), "SPA support plugin", "file");
  requirePath(
    join(moduleDir, "libpipewire-module-protocol-native.so"),
    "native protocol module",
    "file",
  );
  requirePath(join(configDir, "client.conf"), "client config", "file");

  rmSync(stageRoot, { recursive: true, force: true });
  mkdirSync(join(stageRoot, "usr/lib"), { recursive: true });
  mkdirSync(join(stageRoot, "usr/share"), { recursive: true });

  const pipewireLibraries = readdirSync(libDir).filter((name) =>
    name.startsWith("libpipewire-0.3.so"),
  );
  const jackLibraries = readdirSync(libDir).filter((name) =>
    name.startsWith("libjack.so"),
  );
  if (pipewireLibraries.length === 0) {
    throw new Error(`no libpipewire-0.3.so* files found under ${libDir}`);
  }
  if (!jackLibraries.includes("libjack.so.0")) {
    throw new Error(`no libjack.so.0 found under ${libDir}`);
  }
  for (const name of [...pipewireLibraries, ...jackLibraries]) {
    cpSync(join(libDir, name), join(stageRoot, "usr/lib", name), {
      dereference: false,
      force: true,
      verbatimSymlinks: true,
    });
  }
  cpSync(spaDir, join(stageRoot, "usr/lib/spa-0.2"), {
    dereference: false,
    force: true,
    recursive: true,
    verbatimSymlinks: true,
  });
  cpSync(moduleDir, join(stageRoot, "usr/lib/pipewire-0.3"), {
    dereference: false,
    force: true,
    recursive: true,
    verbatimSymlinks: true,
  });
  cpSync(configDir, join(stageRoot, "usr/share/pipewire"), {
    dereference: false,
    force: true,
    recursive: true,
    verbatimSymlinks: true,
  });

  const version = pkgConfig("--modversion", "libpipewire-0.3");
  console.log(`Staged PipeWire ${version} runtime under ${stageRoot}`);
} catch (error) {
  console.error(`stage-pipewire-appimage: ${error.message}`);
  process.exitCode = 1;
}
