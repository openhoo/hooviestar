import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
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
