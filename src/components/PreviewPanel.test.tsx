// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { PreviewPanel } from "./PreviewPanel";

vi.mock("../platform", () => ({ isWindowsPlatform: () => true }));

function renderTransformPreview({
  rotationDegrees = 0,
  locked = false,
}: { rotationDegrees?: number; locked?: boolean } = {}) {
  const onTransform = vi.fn();
  const item = {
    id: "item",
    sourceId: "source",
    visible: true,
    locked,
    transform: {
      x: 100,
      y: 100,
      width: 200,
      height: 100,
      rotationDegrees,
      cropTop: 0,
      cropRight: 0,
      cropBottom: 0,
      cropLeft: 0,
      opacity: 1,
    },
  };
  const view = render(
    <PreviewPanel
      output={{ width: 1000, height: 500, fps: 60, background: "#000000" }}
      activeSceneName="Spiel"
      scene={{ id: "scene", name: "Spiel", hotkey: null, items: [item] }}
      sources={[{ id: "source", name: "Logo", type: "image", path: "/tmp/logo.png" }]}
      selectedSourceId="source"
      onSelectSource={vi.fn()}
      onTransform={onTransform}
      onAttachBounds={vi.fn()}
    />,
  );
  const preview = screen.getByLabelText("Native Szenenvorschau") as HTMLElement;
  Object.defineProperty(preview, "getBoundingClientRect", {
    configurable: true,
    value: () => ({ left: 0, top: 0, width: 1000, height: 500, right: 1000, bottom: 500 }),
  });
  return { ...view, onTransform, item, preview };
}

describe("PreviewPanel output projection and transforms", () => {
  afterEach(cleanup);

  it("projects authoritative aspect ratio and background into preview bounds", () => {
    render(
      <PreviewPanel
        output={{ width: 1920, height: 1080, fps: 60, background: "#335577" }}
        activeSceneName="Spiel"
        scene={{ id: "scene", name: "Spiel", hotkey: null, items: [] }}
        sources={[]}
        selectedSourceId={null}
        onSelectSource={vi.fn()}
        onTransform={vi.fn()}
        onAttachBounds={vi.fn()}
      />,
    );
    const preview = screen.getByLabelText("Native Szenenvorschau") as HTMLElement;
    expect(preview.style.aspectRatio).toBe("1920 / 1080");
    expect(preview.style.backgroundColor).toBe("rgb(51, 85, 119)");
    expect(screen.getByText("Native D3D11-Vorschau")).toBeTruthy();
  });

  it("commits an output-pixel resize through the right handle", async () => {
    const { preview, onTransform } = renderTransformPreview();
    fireEvent.pointerDown(screen.getByRole("button", { name: "Logo Größe rechts" }), {
      button: 0,
      pointerId: 1,
      clientX: 300,
      clientY: 150,
    });
    fireEvent.pointerMove(preview, { pointerId: 1, clientX: 350, clientY: 150 });
    fireEvent.pointerUp(preview, { pointerId: 1, clientX: 350, clientY: 150 });
    await waitFor(() => expect(onTransform).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(onTransform.mock.calls[0]?.[1]).toMatchObject({ x: 100, y: 100, width: 250, height: 100 }));
  });

  it("keeps the opposite anchor fixed for a rotated resize", async () => {
    const { preview, onTransform } = renderTransformPreview({ rotationDegrees: 90 });
    fireEvent.pointerDown(screen.getByRole("button", { name: "Logo Größe rechts" }), {
      button: 0,
      pointerId: 1,
      clientX: 200,
      clientY: 250,
    });
    fireEvent.pointerMove(preview, { pointerId: 1, clientX: 200, clientY: 300 });
    fireEvent.pointerUp(preview, { pointerId: 1, clientX: 200, clientY: 300 });
    await waitFor(() => expect(onTransform).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(onTransform.mock.calls[0]?.[1]).toMatchObject({ x: 75, y: 125, width: 250, height: 100 }));
  });

  it("does not commit a drag that returns to its starting point", () => {
    const { preview, onTransform } = renderTransformPreview();
    fireEvent.pointerDown(screen.getByRole("button", { name: "Logo" }), {
      button: 0,
      pointerId: 1,
      clientX: 200,
      clientY: 150,
    });
    fireEvent.pointerMove(preview, { pointerId: 1, clientX: 240, clientY: 150 });
    fireEvent.pointerMove(preview, { pointerId: 1, clientX: 200, clientY: 150 });
    fireEvent.pointerUp(preview, { pointerId: 1, clientX: 200, clientY: 150 });
    expect(onTransform).not.toHaveBeenCalled();
  });

  it("cancels a focused resize handle with Escape", () => {
    const { preview, onTransform } = renderTransformPreview();
    fireEvent.pointerDown(screen.getByRole("button", { name: "Logo Größe rechts" }), {
      button: 0,
      pointerId: 1,
      clientX: 300,
      clientY: 150,
    });
    fireEvent.pointerMove(preview, { pointerId: 1, clientX: 350, clientY: 150 });
    fireEvent.keyDown(preview, { key: "Escape" });
    fireEvent.pointerUp(preview, { pointerId: 1, clientX: 350, clientY: 150 });
    expect(onTransform).not.toHaveBeenCalled();
  });

  it("uses signed output-size steps for rotated Alt-arrow resize", async () => {
    const { preview, onTransform } = renderTransformPreview({ rotationDegrees: 90 });
    fireEvent.keyDown(preview, { key: "ArrowLeft", altKey: true });
    fireEvent.keyDown(preview, { key: "ArrowRight", altKey: true, shiftKey: true });
    await waitFor(() => expect(onTransform).toHaveBeenCalledTimes(2));
    expect(onTransform.mock.calls[0]?.[1]).toMatchObject({ width: 199, height: 100 });
    expect(onTransform.mock.calls[1]?.[1]).toMatchObject({ width: 209, height: 100 });
  });

  it("does not mutate a locked source from pointer or keyboard input", () => {
    const { preview, onTransform } = renderTransformPreview({ locked: true });
    fireEvent.pointerDown(screen.getByRole("button", { name: "Logo (gesperrt)" }), {
      button: 0,
      pointerId: 1,
      clientX: 200,
      clientY: 150,
    });
    fireEvent.pointerMove(preview, { pointerId: 1, clientX: 240, clientY: 150 });
    fireEvent.pointerUp(preview, { pointerId: 1, clientX: 240, clientY: 150 });
    fireEvent.keyDown(preview, { key: "ArrowRight" });
    expect(onTransform).not.toHaveBeenCalled();
  });
  it("uses the acknowledged snapshot as the next keyboard baseline", async () => {
    const first = renderTransformPreview();
    fireEvent.keyDown(first.preview, { key: "ArrowRight" });
    await waitFor(() => expect(first.onTransform.mock.calls[0]?.[1]).toMatchObject({ x: 101 }));
    const updatedItem = {
      ...first.item,
      transform: { ...first.item.transform, x: 101 },
    };
    first.rerender(
      <PreviewPanel
        output={{ width: 1000, height: 500, fps: 60, background: "#000000" }}
        activeSceneName="Spiel"
        scene={{ id: "scene", name: "Spiel", hotkey: null, items: [updatedItem] }}
        sources={[{ id: "source", name: "Logo", type: "image", path: "/tmp/logo.png" }]}
        selectedSourceId="source"
        onSelectSource={vi.fn()}
        onTransform={first.onTransform}
        onAttachBounds={vi.fn()}
      />,
    );
    fireEvent.keyDown(first.preview, { key: "ArrowRight" });
    await waitFor(() => expect(first.onTransform.mock.calls[1]?.[1]).toMatchObject({ x: 102 }));
  });
});
