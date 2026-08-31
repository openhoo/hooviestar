import { describe, expect, it } from "vitest";
import {
  isHexColor,
  isOutputFrameRate,
  outputResolutionValue,
  parseOutputResolution,
  sameOutput,
} from "./outputSettings";

describe("output settings contract", () => {
  it("accepts only selectable resolutions and frame rates", () => {
    expect(parseOutputResolution("1280x720")).toEqual({ width: 1280, height: 720 });
    expect(parseOutputResolution("1920x1080")).toEqual({ width: 1920, height: 1080 });
    expect(parseOutputResolution("3840x2160")).toBeNull();
    expect(outputResolutionValue({ width: 1920, height: 1080 })).toBe("1920x1080");
    expect(isOutputFrameRate(30)).toBe(true);
    expect(isOutputFrameRate(60)).toBe(true);
    expect(isOutputFrameRate(59.94)).toBe(false);
  });

  it("validates exact six-digit RGB colors", () => {
    expect(isHexColor("#101418")).toBe(true);
    expect(isHexColor("#A0b1C2")).toBe(true);
    for (const invalid of ["101418", "#fff", "#101418ff", "#gg0000", "#10141 "]) {
      expect(isHexColor(invalid)).toBe(false);
    }
  });

  it("compares semantically equal output colors case-insensitively", () => {
    expect(sameOutput(
      { width: 1280, height: 720, fps: 30, background: "#AABBCC" },
      { width: 1280, height: 720, fps: 30, background: "#aabbcc" },
    )).toBe(true);
  });
});
