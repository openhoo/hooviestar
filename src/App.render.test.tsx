// @vitest-environment jsdom
/**
 * Render-Smoke des neuen OBS-artigen Shell-Layouts: Docks vorhanden,
 * Szenenwechsel dispatcht set_active_scene, Quellen-Dock listet Szene+Global.
 * Die Tauri-Grenze wird gemockt; das Projekt-Fixture ist dasselbe shared
 * Rust-Fixture wie in contracts.test.ts.
 */
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import fixture from "../contracts/project-v1.json";
import { parseProjectV1 } from "./types";

const dispatchedCommands: Array<Record<string, unknown>> = [];

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (command: string, args?: Record<string, unknown>) => {
    if (command === "get_snapshot") return structuredClone(fixture);
    if (command === "dispatch") {
      dispatchedCommands.push((args?.command ?? {}) as Record<string, unknown>);
      return null;
    }
    return null;
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => undefined),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(async () => null),
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: vi.fn(() => ({ close: vi.fn() })),
}));

const { default: App } = await import("./App");
const project = parseProjectV1(fixture)!;
async function renderStudio() {
  render(<App />);
  // Engine-Start ist asynchron; warten bis das Szenen-Dock gerendert ist.
  await screen.findByRole("heading", { name: "Szenen" });
}

function sceneRowButton(sceneName: string): HTMLButtonElement {
  // Szenennamen können auch als Quellnamen vorkommen – deshalb auf die
  // Szenenzeile (.scene-row im Szenen-Dock) einschränken.
  const row = screen
    .getAllByText(sceneName)
    .map((node) => node.closest("button.scene-row"))
    .find((node): node is HTMLButtonElement => node !== null);
  expect(row).toBeTruthy();
  return row!;
}

describe("studio shell", () => {
  afterEach(cleanup);

  it("rendert die OBS-Docks Szenen, Quellen, Audio-Mixer, Eigenschaften und Steuerpult", async () => {
    await renderStudio();
    for (const dock of ["Szenen", "Quellen", "Audio-Mixer", "Eigenschaften", "Steuerpult"]) {
      expect(screen.getByRole("heading", { name: dock })).toBeTruthy();
    }
    const counts = screen.getByText((_, element) =>
      element?.classList.contains("status-item") === true &&
      /\b1 (Szene|Szenen) · 6 Quellen\b/.test(element.textContent ?? ""),
    );
    expect(counts).toBeTruthy();
  });

  it("wechselt die Szene über das Szenen-Dock per set_active_scene", async () => {
    await renderStudio();
    const target = project.scenes[project.scenes.length - 1];
    fireEvent.click(sceneRowButton(target.name));
    await waitFor(() => {
      expect(dispatchedCommands).toContainEqual(expect.objectContaining({ type: "set_active_scene", sceneId: target.id }));
    });
  });

  it("listet im Quellen-Dock Elemente der aktiven Szene und außerhalb liegende Quellen", async () => {
    await renderStudio();
    const active = project.scenes.find((scene) => scene.id === project.activeSceneId)!;
    const placedIds = new Set(active.items.map((item) => item.sourceId));
    for (const item of active.items.slice(0, 2)) {
      const source = project.sources.find((entry) => entry.id === item.sourceId)!;
      expect(screen.getAllByText(source.name).length).toBeGreaterThan(0);
    }
    const unplaced = project.sources.filter((source) => !placedIds.has(source.id));
    if (unplaced.length > 0) {
      expect(screen.getAllByText("außerhalb der Szene").length).toBe(unplaced.length);
    }
  });
});
