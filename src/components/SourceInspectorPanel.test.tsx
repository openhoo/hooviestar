// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApplicationAudioSource, MediaRuntimeState, MediaSource, SceneItem, Transform } from "../types";
import { SourceInspectorPanel } from "./SourceInspectorPanel";

const transform: Transform = {
  x: 0,
  y: 0,
  width: 640,
  height: 360,
  rotationDegrees: 0,
  cropTop: 0,
  cropRight: 0,
  cropBottom: 0,
  cropLeft: 0,
  opacity: 1,
};

const media: MediaSource = {
  type: "media",
  id: "00000000-0000-4000-8000-000000000001",
  name: "Video",
  path: "/media/video.mp4",
  loop: false,
  continueWhenHidden: false,
  restartOnShow: false,
  volume: 1,
  muted: false,
};

const applicationAudio: ApplicationAudioSource = {
  type: "application_audio",
  id: "00000000-0000-4000-8000-000000000004",
  name: "Discord-Audio",
  binding: { processPath: "/apps/discord.exe", sessionGroupingId: "discord-session" },
  volume: 0.75,
  muted: false,
};

const item: SceneItem = {
  id: "00000000-0000-4000-8000-000000000002",
  sourceId: media.id,
  visible: true,
  locked: false,
  transform,
};

const mediaState: MediaRuntimeState = {
  playing: false,
  positionSeconds: 0,
  durationSeconds: 30,
};

describe("SourceInspectorPanel", () => {
  afterEach(cleanup);

  it("gives the inspector audio slider an accessible source-specific name", () => {
    render(
      <SourceInspectorPanel
        selectedSource={media}
        selectedItem={item}
        mediaState={mediaState}
        textError={null}
        onTextChange={vi.fn()}
        onAudioField={vi.fn()}
        getPendingField={(_sourceId, _field, fallback) => fallback}
        onUpdateSource={vi.fn(async () => undefined)}
        onUpdateTransform={vi.fn()}
        onSeek={vi.fn(async () => undefined)}
        onSetPlaying={vi.fn(async () => undefined)}
      />,
    );

    expect(screen.getByRole("slider", { name: "Lautstärke Video" })).toBeTruthy();
  });

  it("gives application-audio inspector sliders the same accessible naming", () => {
    render(
      <SourceInspectorPanel
        selectedSource={applicationAudio}
        selectedItem={null}
        mediaState={null}
        textError={null}
        onTextChange={vi.fn()}
        onAudioField={vi.fn()}
        getPendingField={(_sourceId, _field, fallback) => fallback}
        onUpdateSource={vi.fn(async () => undefined)}
        onUpdateTransform={vi.fn()}
        onSeek={vi.fn(async () => undefined)}
        onSetPlaying={vi.fn(async () => undefined)}
      />,
    );

    expect(screen.getByRole("slider", { name: "Lautstärke Discord-Audio" })).toBeTruthy();
  });
});
