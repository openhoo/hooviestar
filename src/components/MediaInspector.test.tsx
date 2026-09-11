// @vitest-environment jsdom
/**
 * Mediensteuerung im Eigenschaften-Dock: der Wiedergabe-Knopf spiegelt den
 * Medienstatus und dispatcht set_media_playing mit dem invertierten Wert.
 * Die Tauri-Grenze wird wie in App.render.test.tsx gemockt.
 */
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
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

function deferred<T>() {
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((_resolve, rejectPromise) => {
    reject = rejectPromise;
  });
  return { promise, reject };
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

  it("verwirft Positionsfehler und -entwurf beim Wechsel auf eine andere Quelle", async () => {
    const onSeek = vi.fn(async () => {
      throw new Error("seek failed");
    });
    const view = render(
      <MediaInspector
        source={source}
        mediaState={mediaState(true)}
        onUpdateSource={vi.fn(async () => undefined)}
        onSeek={onSeek}
        onSetPlaying={vi.fn(async () => undefined)}
      />,
    );
    const position = screen.getByRole("spinbutton");
    fireEvent.focus(position);
    fireEvent.change(position, { target: { value: "12" } });
    fireEvent.blur(position);
    expect((await screen.findByRole("alert")).textContent).toContain("seek failed");

    const replacement = { ...source, id: "00000000-0000-4000-8000-000000000003", name: "Ersatz" };
    view.rerender(
      <MediaInspector
        source={replacement}
        mediaState={mediaState(false)}
        onUpdateSource={vi.fn(async () => undefined)}
        onSeek={vi.fn(async () => undefined)}
        onSetPlaying={vi.fn(async () => undefined)}
      />,
    );

    await waitFor(() => {
      expect(screen.queryByRole("alert")).toBeNull();
      expect((screen.getByRole("spinbutton") as HTMLInputElement).value).toBe("3");
    });
  });

  it("ignoriert eine verspätete Positionsfehler-Antwort der vorherigen Quelle", async () => {
    const pending = deferred<void>();
    const onSeek = vi.fn(() => pending.promise);
    const view = render(
      <MediaInspector
        source={source}
        mediaState={mediaState(true)}
        onUpdateSource={vi.fn(async () => undefined)}
        onSeek={onSeek}
        onSetPlaying={vi.fn(async () => undefined)}
      />,
    );
    const position = screen.getByRole("spinbutton");
    fireEvent.focus(position);
    fireEvent.change(position, { target: { value: "12" } });
    fireEvent.blur(position);

    const replacement = { ...source, id: "00000000-0000-4000-8000-000000000005", name: "Ersatz" };
    view.rerender(
      <MediaInspector
        source={replacement}
        mediaState={mediaState(false)}
        onUpdateSource={vi.fn(async () => undefined)}
        onSeek={vi.fn(async () => undefined)}
        onSetPlaying={vi.fn(async () => undefined)}
      />,
    );
    await waitFor(() => expect((screen.getByRole("spinbutton") as HTMLInputElement).value).toBe("3"));

    await act(async () => {
      pending.reject(new Error("alter Seek fehlgeschlagen"));
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("ignoriert eine verspätete Aktionsfehler-Antwort der vorherigen Quelle", async () => {
    const pending = deferred<void>();
    const onSetPlaying = vi.fn(() => pending.promise);
    const view = render(
      <MediaInspector
        source={source}
        mediaState={mediaState(true)}
        onUpdateSource={vi.fn(async () => undefined)}
        onSeek={vi.fn(async () => undefined)}
        onSetPlaying={onSetPlaying}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Pause" }));
    expect(onSetPlaying).toHaveBeenCalledWith(source.id, false);

    const replacement = { ...source, id: "00000000-0000-4000-8000-000000000006", name: "Ersatz" };
    view.rerender(
      <MediaInspector
        source={replacement}
        mediaState={mediaState(false)}
        onUpdateSource={vi.fn(async () => undefined)}
        onSeek={vi.fn(async () => undefined)}
        onSetPlaying={vi.fn(async () => undefined)}
      />,
    );

    await act(async () => {
      pending.reject(new Error("alte Aktion fehlgeschlagen"));
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(screen.queryByRole("alert")).toBeNull();
  });
});
