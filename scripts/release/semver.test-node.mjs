import assert from "node:assert/strict";
import test from "node:test";
import { compareReleaseSemver, isReleaseSemver } from "./semver.mjs";

test("accepts stable and prerelease versions supported by the release workflow", () => {
  for (const version of ["0.1.0", "1.0.0", "12.34.56", "2.0.0-rc.1", "1.2.3-alpha-7"]) {
    assert.equal(isReleaseSemver(version), true, version);
  }
});

test("compares versions using SemVer precedence", () => {
  const ordered = [
    "1.0.0-alpha",
    "1.0.0-alpha.1",
    "1.0.0-alpha.beta",
    "1.0.0-beta",
    "1.0.0-beta.2",
    "1.0.0-beta.11",
    "1.0.0-rc.1",
    "1.0.0",
    "1.0.1",
    "1.1.0",
    "2.0.0",
  ];
  for (let index = 1; index < ordered.length; index += 1) {
    assert.equal(compareReleaseSemver(ordered[index - 1], ordered[index]), -1);
    assert.equal(compareReleaseSemver(ordered[index], ordered[index - 1]), 1);
  }
  assert.equal(compareReleaseSemver("2.3.4", "2.3.4"), 0);
  assert.equal(compareReleaseSemver("2.3.4-alpha-7", "2.3.4-alpha-8"), -1);
});

test("rejects malformed, ambiguous, and build-metadata versions", () => {
  for (const version of [
    "v1.2.3",
    "01.2.3",
    "1.02.3",
    "1.2.03",
    "1.2",
    "1.2.3-",
    "1.2.3-rc..1",
    "1.2.3-01",
    "1.2.3+build.1",
  ]) {
    assert.equal(isReleaseSemver(version), false, version);
  }
});
