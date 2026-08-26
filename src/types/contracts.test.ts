import { describe, expect, it } from "vitest";
import fixture from "../../contracts/project-v1.json";
import { parseProjectV1 } from "./project";
import { ENGINE_COMMAND_TYPES, ENGINE_EVENT_TYPES, parseEngineEvent } from "./engine";
import commandSamples from "../../contracts/commands-v1.json";
import eventSamples from "../../contracts/events-v1.json";

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
  it("rejects snapshot events with a malformed project payload", () => {
    const broken = structuredClone(fixture);
    broken.activeSceneId = "00000000-0000-4000-8000-0000000000ff";
    expect(parseEngineEvent({ type: "snapshot", project: broken })).toBeNull();
  });
  it("rejects device_recovery events with an invalid phase", () => {
    expect(parseEngineEvent({ type: "device_recovery", phase: "paused", detail: null })).toBeNull();
  });
  it("rejects levels entries missing rms", () => {
    expect(
      parseEngineEvent({
        type: "levels",
        entries: [{ sourceId: "00000000-0000-4000-8000-000000000001", peak: 0.5 }],
      }),
    ).toBeNull();
  });
  it("rejects unknown event types", () => {
    expect(parseEngineEvent({ type: "scene_teleport" })).toBeNull();
  });
  it("rejects scene items referencing unknown sources", () => {
    const project = structuredClone(fixture);
    project.scenes[0].items[0].sourceId = "00000000-0000-4000-8000-0000000000ff";
    expect(parseProjectV1(project)).toBeNull();
  });
  it("positively parses every pinned event sample from the shared Rust fixture", () => {
    const samples = eventSamples as unknown as { type: string }[];
    expect(samples.map((sample) => sample.type)).toEqual([...ENGINE_EVENT_TYPES]);
    for (const sample of samples) {
      const event = parseEngineEvent(sample);
      expect(event).not.toBeNull();
      expect(event?.type).toBe(sample.type);
    }
  });
  it("pins every command tag in the shared Rust fixture exactly once", () => {
    const tags = (commandSamples as unknown as { type: string }[]).map((sample) => sample.type);
    expect(tags).toEqual([...ENGINE_COMMAND_TYPES]);
  });
});
