import { compareReleaseSemver, isReleaseSemver } from "./semver.mjs";

const [candidate, published] = process.argv.slice(2);
if (!isReleaseSemver(candidate) || !isReleaseSemver(published)) {
  throw new Error("candidate and published versions must be strict SemVer");
}
if (candidate.includes("-") || published.includes("-")) {
  throw new Error("stable release ordering only accepts stable versions");
}
if (compareReleaseSemver(candidate, published) <= 0) {
  throw new Error(`stable release ${candidate} must be newer than published release ${published}`);
}
console.log(`stable release ${candidate} is newer than ${published}`);
