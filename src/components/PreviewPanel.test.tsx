// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { PreviewPanel } from "./PreviewPanel";

vi.mock("../platform", () => ({ isWindowsPlatform: () => true }));

describe("PreviewPanel output projection", () => {
  afterEach(cleanup);

  it("projects authoritative aspect ratio and background into preview bounds", () => {
    render(
      <PreviewPanel
        output={{ width: 1920, height: 1080, fps: 60, background: "#335577" }}
        activeSceneName="Spiel"
        onAttachBounds={vi.fn()}
      />,
    );
    const preview = screen.getByLabelText("Native Szenenvorschau") as HTMLElement;
    expect(preview.style.aspectRatio).toBe("1920 / 1080");
    expect(preview.style.backgroundColor).toBe("rgb(51, 85, 119)");
    expect(screen.getByText("Native D3D11-Vorschau")).toBeTruthy();
  });
});
