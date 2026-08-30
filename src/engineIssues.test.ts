import { describe, expect, it } from "vitest";
import fixture from "../contracts/project-v1.json";
import { EngineIssueTracker } from "./engineIssues";
import { parseProjectV1 } from "./types";

const sourceA = "00000000-0000-4000-8000-0000000000a1";
const sourceB = "00000000-0000-4000-8000-0000000000a2";

describe("EngineIssueTracker", () => {
  it("clears only the source that recovered", () => {
    const issues = new EngineIssueTracker();
    issues.setStartup(null);
    issues.record({ type: "source_unavailable", sourceId: sourceA, reason: "A offline" });
    issues.record({ type: "source_unavailable", sourceId: sourceB, reason: "B offline" });
    issues.record({ type: "source_available", sourceId: sourceA });
    expect(issues.status()).toBe("Quelle nicht verfügbar: B offline");
    issues.record({ type: "source_available", sourceId: sourceB });
    expect(issues.status()).toBe("Program bereit");
  });

  it("does not let source recovery hide a device failure", () => {
    const issues = new EngineIssueTracker();
    issues.setStartup(null);
    issues.record({ type: "device_recovery", phase: "failed", detail: "device lost" });
    issues.record({ type: "source_available", sourceId: sourceA });
    expect(issues.status()).toBe("Grafikfehler: device lost");
    issues.record({ type: "device_recovery", phase: "succeeded", detail: null });
    expect(issues.status()).toBe("Program bereit");
  });

  it("prunes issues for objects removed by a snapshot", () => {
    const project = parseProjectV1(fixture)!;
    const issues = new EngineIssueTracker();
    issues.setStartup(null);
    issues.record({ type: "source_unavailable", sourceId: sourceA, reason: "gone" });
    issues.record({ type: "hotkey_error", sceneId: sourceB, message: "gone" });
    issues.record({ type: "snapshot", project });
    expect(issues.status()).toBe("Program bereit");
  });

  it("clears a transient audio warning only after explicit audio recovery", () => {
    const issues = new EngineIssueTracker();
    issues.setStartup(null);
    issues.record({ type: "audio_warning", kind: "device_invalidated", message: "offline" });
    issues.record({ type: "levels", entries: [{ sourceId: sourceA, peak: 0.5, rms: 0.2 }] });
    expect(issues.status()).toBe("Audiowarnung: offline");
    issues.record({ type: "audio_recovered" });
    expect(issues.status()).toBe("Program bereit");
  });
});
