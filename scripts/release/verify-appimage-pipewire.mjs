import { execFileSync } from "node:child_process";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative } from "node:path";

const probeSource = `
#include <dlfcn.h>
#include <pipewire/pipewire.h>
#include <stdio.h>

int main(int argc, char **argv) {
    pw_init(NULL, NULL);
    struct pw_main_loop *loop = pw_main_loop_new(NULL);
    if (loop == NULL) {
        fputs("pw_main_loop_new failed\\n", stderr);
        return 1;
    }
    struct pw_properties *properties =
        pw_properties_new(PW_KEY_CONFIG_NAME, "client.conf", NULL);
    if (properties == NULL) {
        fputs("pw_properties_new(client.conf) failed\\n", stderr);
        pw_main_loop_destroy(loop);
        return 1;
    }
    struct pw_context *context =
        pw_context_new(pw_main_loop_get_loop(loop), properties, 0);
    if (context == NULL) {
        fputs("pw_context_new(client.conf) failed\\n", stderr);
        pw_main_loop_destroy(loop);
        return 1;
    }
    if (argc > 1) {
        void *plugin = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
        if (plugin == NULL) {
            fprintf(stderr, "dlopen(%s) failed: %s\\n", argv[1], dlerror());
            pw_context_destroy(context);
            pw_main_loop_destroy(loop);
            return 1;
        }
        dlclose(plugin);
    }
    pw_context_destroy(context);
    pw_main_loop_destroy(loop);
    pw_deinit();
    puts("PipeWire client context and fluidsynth GStreamer plugin loaded");
    return 0;
}
`;

const appDir = process.argv[2];
if (appDir === undefined || !existsSync(join(appDir, "usr/bin/hooviestar"))) {
  throw new Error("usage: verify-appimage-pipewire.mjs <extracted-AppDir>");
}

const appLib = join(appDir, "usr/lib");
const requiredFiles = [
  join(appLib, "libpipewire-0.3.so.0"),
  join(appLib, "libjack.so.0"),
  join(appLib, "spa-0.2/support/libspa-support.so"),
  join(appLib, "pipewire-0.3/libpipewire-module-protocol-native.so"),
  join(appDir, "usr/share/pipewire/client.conf"),
];
for (const path of requiredFiles) {
  if (!existsSync(path)) {
    throw new Error(`required packaged PipeWire file is missing: ${path}`);
  }
}
const appRun = join(appDir, "AppRun");
const appRunShell = join(appDir, "AppRun.shell");
const appRunWrapped = join(appDir, "AppRun.wrapped");
for (const [path, description] of [
  [appRun, "static AppRun bootstrap"],
  [appRunShell, "generated AppRun shell"],
  [appRunWrapped, "wrapped native AppRun"],
]) {
  if (!existsSync(path) || (statSync(path).mode & 0o111) === 0) {
    throw new Error(`${description} is missing or not executable: ${path}`);
  }
}
if (!readFileSync(appRun).subarray(0, 4).equals(Buffer.from([0x7f, 0x45, 0x4c, 0x46]))) {
  throw new Error(`AppRun is not a native ELF bootstrap: ${appRun}`);
}
const programHeaders = execFileSync("readelf", ["-l", appRun], { encoding: "utf8" });
if (/\bINTERP\b/.test(programHeaders)) {
  throw new Error(`AppRun bootstrap is dynamically linked: ${appRun}`);
}
const generatedShell = readFileSync(appRunShell, "utf8");
if (!generatedShell.includes("AppRun.wrapped")) {
  throw new Error(`generated AppRun shell no longer delegates to AppRun.wrapped: ${appRunShell}`);
}

const bootstrapProbeDir = mkdtempSync(join(tmpdir(), "hooviestar-apprun-bootstrap-"));
try {
  const probeAppRun = join(bootstrapProbeDir, "AppRun");
  const probeShell = join(bootstrapProbeDir, "AppRun.shell");
  copyFileSync(appRun, probeAppRun);
  chmodSync(probeAppRun, 0o755);
  writeFileSync(
    probeShell,
    '#!/bin/sh\nprintf "%s\\n%s\\n%s\\n%s\\n" "${APPDIR-}" "${LD_LIBRARY_PATH-unset}" "${APPIMAGE-}" "$1"\n',
    { mode: 0o755 },
  );
  const expectedAppImage = "/tmp/hooviestar-old.AppImage";
  const poisonedEnvironment = {
    ...process.env,
    APPDIR: "/tmp/old-hooviestar-mount",
    APPIMAGE: expectedAppImage,
    LD_LIBRARY_PATH: appLib,
  };
  const inheritedOutput = execFileSync(probeAppRun, ["preserved-argument"], {
    encoding: "utf8",
    env: poisonedEnvironment,
  })
    .trimEnd()
    .split("\n");
  if (
    inheritedOutput[0] !== bootstrapProbeDir ||
    inheritedOutput[1] !== "unset" ||
    inheritedOutput[2] !== expectedAppImage ||
    inheritedOutput[3] !== "preserved-argument"
  ) {
    throw new Error(`AppRun bootstrap did not derive/sanitize the inherited environment: ${inheritedOutput}`);
  }
  const { APPDIR: _ignoredAppDir, ...withoutAppDir } = poisonedEnvironment;
  const fallbackOutput = execFileSync(probeAppRun, ["fallback-argument"], {
    encoding: "utf8",
    env: withoutAppDir,
  })
    .trimEnd()
    .split("\n");
  if (
    fallbackOutput[0] !== bootstrapProbeDir ||
    fallbackOutput[1] !== "unset" ||
    fallbackOutput[2] !== expectedAppImage ||
    fallbackOutput[3] !== "fallback-argument"
  ) {
    throw new Error(`AppRun bootstrap did not derive APPDIR or preserve arguments: ${fallbackOutput}`);
  }
} finally {
  rmSync(bootstrapProbeDir, { recursive: true, force: true });
}

const plugin = join(appDir, "usr/lib/gstreamer-1.0/libgstfluidsynthmidi.so");
if (!existsSync(plugin)) {
  throw new Error(`bundled GStreamer fluidsynth plugin is missing: ${plugin}`);
}

const tempDir = mkdtempSync(join(tmpdir(), "hooviestar-appimage-pipewire-"));
try {
  const source = join(tempDir, "probe.c");
  const binary = join(tempDir, "probe");
  writeFileSync(source, probeSource);
  const pkgConfig = execFileSync(
    "pkg-config",
    ["--cflags", "--libs", "libpipewire-0.3"],
    { encoding: "utf8" },
  )
    .trim()
    .split(/\s+/)
    .filter(Boolean);
  execFileSync("cc", [source, "-o", binary, ...pkgConfig, "-ldl"], {
    stdio: "inherit",
  });

  const appArchLib = join(appDir, "usr/lib/x86_64-linux-gnu");
  const env = {
    ...process.env,
    APPDIR: appDir,
    SPA_PLUGIN_DIR: join(appLib, "spa-0.2"),
    PIPEWIRE_MODULE_DIR: join(appLib, "pipewire-0.3"),
    PIPEWIRE_CONFIG_DIR: join(appDir, "usr/share/pipewire"),
    PIPEWIRE_CONFIG_NAME: "client.conf",
    LD_LIBRARY_PATH: [appLib, appArchLib].join(":"),
  };
  execFileSync(binary, [plugin], { env, stdio: "inherit" });
  console.log(`Verified bundled PipeWire runtime from ${relative(dirname(appDir), appDir)}`);
} finally {
  rmSync(tempDir, { recursive: true, force: true });
}
