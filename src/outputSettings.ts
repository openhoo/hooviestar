import type { OutputConfig } from "./types";

export const OUTPUT_RESOLUTIONS = [
  { width: 1280, height: 720, label: "1280 × 720", description: "HD · geringere GPU- und Netzwerkbelastung" },
  { width: 1920, height: 1080, label: "1920 × 1080", description: "Full HD · schärferes Bild" },
] as const;

export const OUTPUT_FRAME_RATES = [30, 60] as const;

export function outputResolutionValue(output: Pick<OutputConfig, "width" | "height">): string {
  return `${output.width}x${output.height}`;
}

export function parseOutputResolution(value: string): Pick<OutputConfig, "width" | "height"> | null {
  const resolution = OUTPUT_RESOLUTIONS.find((entry) => `${entry.width}x${entry.height}` === value);
  return resolution ? { width: resolution.width, height: resolution.height } : null;
}

export function isOutputFrameRate(value: number): value is (typeof OUTPUT_FRAME_RATES)[number] {
  return OUTPUT_FRAME_RATES.some((fps) => fps === value);
}

export function isHexColor(value: string): boolean {
  return /^#[0-9a-fA-F]{6}$/.test(value);
}

export function sameOutput(a: OutputConfig, b: OutputConfig): boolean {
  return a.width === b.width && a.height === b.height && a.fps === b.fps &&
    a.background.toLowerCase() === b.background.toLowerCase();
}
