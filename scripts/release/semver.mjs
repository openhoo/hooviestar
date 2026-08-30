const numericIdentifier = "(?:0|[1-9]\\d*)";
const prereleaseIdentifier = "(?:0|[1-9]\\d*|\\d*[A-Za-z-][0-9A-Za-z-]*)";
const releaseSemver = new RegExp(
  `^${numericIdentifier}\\.${numericIdentifier}\\.${numericIdentifier}` +
    `(?:-${prereleaseIdentifier}(?:\\.${prereleaseIdentifier})*)?$`,
);

export function isReleaseSemver(version) {
  return typeof version === "string" && releaseSemver.test(version);
}

export function compareReleaseSemver(left, right) {
  if (!isReleaseSemver(left) || !isReleaseSemver(right)) {
    throw new Error("cannot compare invalid release versions");
  }
  const splitVersion = (version) => {
    const separator = version.indexOf("-");
    return separator === -1
      ? [version, undefined]
      : [version.slice(0, separator), version.slice(separator + 1)];
  };
  const [leftCore, leftPrerelease] = splitVersion(left);
  const [rightCore, rightPrerelease] = splitVersion(right);
  const leftParts = leftCore.split(".").map(BigInt);
  const rightParts = rightCore.split(".").map(BigInt);
  for (let index = 0; index < leftParts.length; index += 1) {
    if (leftParts[index] < rightParts[index]) return -1;
    if (leftParts[index] > rightParts[index]) return 1;
  }
  if (leftPrerelease === undefined && rightPrerelease === undefined) return 0;
  if (leftPrerelease === undefined) return 1;
  if (rightPrerelease === undefined) return -1;

  const leftIdentifiers = leftPrerelease.split(".");
  const rightIdentifiers = rightPrerelease.split(".");
  const count = Math.max(leftIdentifiers.length, rightIdentifiers.length);
  for (let index = 0; index < count; index += 1) {
    const leftIdentifier = leftIdentifiers[index];
    const rightIdentifier = rightIdentifiers[index];
    if (leftIdentifier === undefined) return -1;
    if (rightIdentifier === undefined) return 1;
    if (leftIdentifier === rightIdentifier) continue;
    const leftNumeric = /^\d+$/.test(leftIdentifier);
    const rightNumeric = /^\d+$/.test(rightIdentifier);
    if (leftNumeric && rightNumeric) {
      return BigInt(leftIdentifier) < BigInt(rightIdentifier) ? -1 : 1;
    }
    if (leftNumeric !== rightNumeric) return leftNumeric ? -1 : 1;
    return leftIdentifier < rightIdentifier ? -1 : 1;
  }
  return 0;
}
