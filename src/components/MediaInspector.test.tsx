// @vitest-environment jsdom
/**
 * Mediensteuerung im Eigenschaften-Dock: der Wiedergabe-Knopf spiegelt den
 * Medienstatus und dispatcht set_media_playing mit dem invertierten Wert.
 * Die Tauri-Grenze wird wie in App.render.test.tsx gemockt.
 */
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { MediaRuntimeState, MediaSource } from "../types";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => null),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => undefined),
}));

const { MediaInspector } = await import("./MediaInspector");

const source: MediaSource = {
  type: "media",
  id: "00000000-0000-4000-8000-000000000001",
  name: "Medium",
  path: "/media/clip.mp4",
  loop: false,
  continueWhenHidden: false,
  restartOnShow: false,
  volume: 1,
  muted: false,
};

function mediaState(playing: boolean): MediaRuntimeState {
  return { playing, positionSeconds: 3, durationSeconds: 30 };
}

describe("MediaInspector", () => {
  afterEach(cleanup);

  it("pausiert laufende Wiedergabe über den Pause-Knopf", () => {
    const onSetPlaying = vi.fn(async () => null);
    render(
      <MediaInspector
        source={source}
        mediaState={mediaState(true)}
        onUpdateSource={vi.fn(async () => undefined)}
        onSeek={vi.fn(async () => null)}
        onSetPlaying={onSetPlaying}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Pause" }));
    expect(onSetPlaying).toHaveBeenCalledWith(source.id, false);
  });

  it("startet pausierte Wiedergabe über den Wiedergabe-Knopf", () => {
    const onSetPlaying = vi.fn(async () => null);
    render(
      <MediaInspector
        source={source}
        mediaState={mediaState(false)}
        onUpdateSource={vi.fn(async () => undefined)}
        onSeek={vi.fn(async () => null)}
        onSetPlaying={onSetPlaying}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Wiedergabe" }));
    expect(onSetPlaying).toHaveBeenCalledWith(source.id, true);
  });
});
