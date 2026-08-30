import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const checkConfig = resolve(dirname(fileURLToPath(import.meta.url)), "check-config.mjs");

function runCheck(refType, refName) {
  return spawnSync(process.execPath, [checkConfig], {
    encoding: "utf8",
    env: {
      ...process.env,
      GITHUB_REF_TYPE: refType,
      GITHUB_REF_NAME: refName,
    },
  });
}

test("accepts ordinary branch CI without treating the branch as a release tag", () => {
  const result = runCheck("branch", "main");
  assert.equal(result.status, 0, result.stderr);
});

test("rejects a release tag that differs from the project version", () => {
  const result = runCheck("tag", "v999.0.0");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /does not match version/);
});
