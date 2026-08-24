import { describe, expect, it } from "vitest";
import fixture from "../../contracts/project-v1.json";
import { parseProjectV1 } from "./project";
import { ENGINE_COMMAND_TYPES, ENGINE_EVENT_TYPES, parseEngineEvent } from "./engine";

describe("shared engine contract", () => {
  it("accepts every persisted source variant from Rust fixture", () => {
    const project = parseProjectV1(fixture);
    expect(project).not.toBeNull();
    expect(project?.sources.map((source) => source.type)).toEqual(["window", "display", "image", "text", "media", "application_audio"]);
  });
  it("rejects persisted audio volumes outside the mixer range", () => {
    const project = structuredClone(fixture);
    project.sources[4].volume = 1.01;
    expect(parseProjectV1(project)).toBeNull();
    project.sources[4].volume = 0.5;
    project.sources[5].volume = -0.01;
    expect(parseProjectV1(project)).toBeNull();
  });
  it("keeps command and event tags unique", () => {
    expect(new Set(ENGINE_COMMAND_TYPES).size).toBe(ENGINE_COMMAND_TYPES.length);
    expect(new Set(ENGINE_EVENT_TYPES).size).toBe(ENGINE_EVENT_TYPES.length);
    expect(parseEngineEvent({ type: "audio_warning", kind: "underrun", message: "x" })).toEqual({ type: "audio_warning", kind: "underrun", message: "x" });
    expect(
      parseEngineEvent({
        type: "unsupported_media",
        sourceId: "00000000-0000-4000-8000-000000000001",
        reason: "codec",
      }),
    ).toEqual({
      type: "unsupported_media",
      sourceId: "00000000-0000-4000-8000-000000000001",
      reason: "codec",
    });
  });
});
