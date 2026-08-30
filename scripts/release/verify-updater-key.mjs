import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const privateKey = process.env.TAURI_SIGNING_PRIVATE_KEY;
const privateKeyPassword = process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD;
if (!privateKey || !privateKeyPassword) {
  throw new Error("updater signing key and password are required");
}

const tauriConfig = JSON.parse(readFileSync(join(root, "src-tauri/tauri.conf.json"), "utf8"));
const encodedPublicKey = tauriConfig.plugins?.updater?.pubkey;
if (typeof encodedPublicKey !== "string" || encodedPublicKey.length < 80) {
  throw new Error("committed updater public key is missing");
}

const checkDir = mkdtempSync(join(tmpdir(), "hooviestar-updater-key-"));
try {
  const probe = join(checkDir, "key-check.txt");
  const encodedSignature = `${probe}.sig`;
  const publicKey = join(checkDir, "public.key");
  const signature = join(checkDir, "signature.minisig");
  writeFileSync(probe, "Hooviestar updater signing key check\n", { mode: 0o600 });

  const tauri = join(root, "node_modules", ".bin", process.platform === "win32" ? "tauri.cmd" : "tauri");
  const { TAURI_SIGNING_PRIVATE_KEY_PATH: _ignoredKeyPath, ...cleanEnvironment } = process.env;
  const signed = spawnSync(tauri, ["signer", "sign", probe], {
    cwd: root,
    env: {
      ...cleanEnvironment,
      TAURI_SIGNING_PRIVATE_KEY: privateKey,
      TAURI_SIGNING_PRIVATE_KEY_PASSWORD: privateKeyPassword,
    },
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (signed.status !== 0) {
    throw new Error(`Tauri updater probe signing failed with exit code ${signed.status ?? "unknown"}`);
  }

  const decodedPublicKey = Buffer.from(encodedPublicKey, "base64").toString("utf8");
  const decodedSignature = Buffer.from(readFileSync(encodedSignature, "utf8").trim(), "base64").toString("utf8");
  if (!decodedPublicKey.startsWith("untrusted comment: minisign public key:")) {
    throw new Error("committed updater public key has invalid encoding");
  }
  if (!decodedSignature.startsWith("untrusted comment: signature from tauri secret key")) {
    throw new Error("Tauri updater signature has invalid encoding");
  }
  writeFileSync(publicKey, decodedPublicKey, { mode: 0o600 });
  writeFileSync(signature, decodedSignature, { mode: 0o600 });

  const verified = spawnSync("minisign", ["-Vm", probe, "-p", publicKey, "-x", signature], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (verified.status !== 0) {
    throw new Error("updater private key does not match committed public key");
  }
  console.log("updater private key matches committed public key");
} finally {
  rmSync(checkDir, { recursive: true, force: true });
}
