// @vitest-environment jsdom
/**
 * Render-Smoke des neuen OBS-artigen Shell-Layouts: Docks vorhanden,
 * Szenenwechsel dispatcht set_active_scene, Quellen-Dock listet Szene+Global.
 * Die Tauri-Grenze wird gemockt; das Projekt-Fixture ist dasselbe shared
 * Rust-Fixture wie in contracts.test.ts.
 */
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import fixture from "../contracts/project-v1.json";
import { parseProjectV1 } from "./types";

const dispatchedCommands: Array<Record<string, unknown>> = [];
const invokedCommands: Array<{ command: string; args?: Record<string, unknown> }> = [];
let rejectedDispatchType: string | null = null;
let rejectedInvokeCommand: string | null = null;

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (command: string, args?: Record<string, unknown>) => {
    invokedCommands.push({ command, args });
    if (command === rejectedInvokeCommand) throw new Error("invoke failed");
    if (command === "get_snapshot") return structuredClone(fixture);
    if (command === "dispatch") {
      const dispatched = (args?.command ?? {}) as Record<string, unknown>;
      dispatchedCommands.push(dispatched);
      if (dispatched.type === rejectedDispatchType) throw new Error("dispatch failed");
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

function placedSource() {
  const active = project.scenes.find((scene) => scene.id === project.activeSceneId)!;
  return project.sources.find((entry) => entry.id === active.items[0]!.sourceId)!;
}

function placedSourceRowButton(): HTMLButtonElement {
  const source = placedSource();
  // Quellnamen können mehrfach vorkommen (Mixer, Inspektor) – deshalb auf die
  // Quellenzeile (button.source-main im Quellen-Dock) einschränken.
  const row = screen
    .getAllByText(source.name)
    .map((node) => node.closest("button.source-main"))
    .find((node): node is HTMLButtonElement => node !== null);
  expect(row).toBeTruthy();
  return row!;
}
describe("studio shell", () => {
  beforeEach(() => {
    dispatchedCommands.length = 0;
    invokedCommands.length = 0;
    rejectedDispatchType = null;
    rejectedInvokeCommand = null;
  });
  afterEach(cleanup);

  it("ordnet Szenen und Quellen links, Program und Mixer mittig sowie Eigenschaften rechts an", async () => {
    await renderStudio();
    for (const dock of ["Szenen", "Quellen", "Audio-Mixer", "Eigenschaften", project.scenes[0]!.name]) {
      expect(screen.getByRole("heading", { name: dock })).toBeTruthy();
    }
    expect(screen.queryByRole("heading", { name: "Steuerpult" })).toBeNull();
    expect(screen.getByRole("button", { name: "Studio beenden" })).toBeTruthy();
    const counts = screen.getByText((_, element) =>
      element?.classList.contains("status-item") === true &&
      /\b\d+ (Szene|Szenen) · \d+ (Quelle|Quellen)\b/.test(element.textContent ?? ""),
    );
    expect(counts).toBeTruthy();
  });

  it("öffnet Ausgabe-Einstellungen und dispatcht die vollständige Auswahl genau einmal", async () => {
    await renderStudio();
    fireEvent.click(screen.getByRole("button", { name: "Ausgabe-Einstellungen öffnen" }));
    expect(screen.getByRole("dialog", { name: "Ausgabe-Einstellungen" })).toBeTruthy();

    fireEvent.change(screen.getByLabelText("Auflösung"), { target: { value: "1920x1080" } });
    fireEvent.change(screen.getByLabelText("Bildrate"), { target: { value: "60" } });
    fireEvent.change(screen.getByLabelText("Hintergrundfarbe"), { target: { value: "#224466" } });
    fireEvent.click(screen.getByRole("button", { name: "Anwenden" }));

    await waitFor(() => expect(dispatchedCommands.filter((command) => command.type === "set_output_config")).toEqual([
      {
        type: "set_output_config",
        output: { width: 1920, height: 1080, fps: 60, background: "#224466" },
      },
    ]));
  });

  it("zeigt einen fehlgeschlagenen Output-Commit im offenen Dialog", async () => {
    await renderStudio();
    rejectedDispatchType = "set_output_config";
    fireEvent.click(screen.getByRole("button", { name: "Ausgabe-Einstellungen öffnen" }));
    fireEvent.change(screen.getByLabelText("Bildrate"), { target: { value: "60" } });
    fireEvent.click(screen.getByRole("button", { name: "Anwenden" }));

    expect(await screen.findByText(/Einstellungen konnten nicht angewendet werden: Error: dispatch failed/)).toBeTruthy();
    expect(screen.getByRole("dialog", { name: "Ausgabe-Einstellungen" })).toBeTruthy();
  });

  it("schließt Settings per Escape und gibt Fokus an den Öffner zurück", async () => {
    await renderStudio();
    const opener = screen.getByRole("button", { name: "Ausgabe-Einstellungen öffnen" });
    (opener as HTMLButtonElement).focus();
    fireEvent.click(opener);
    expect(screen.getByRole("dialog", { name: "Ausgabe-Einstellungen" })).toBeTruthy();

    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Ausgabe-Einstellungen" })).toBeNull());
    await waitFor(() => expect(document.activeElement).toBe(opener));
  });

  it("blendet die native Windows-Vorschau hinter allen Webview-Dialogen aus", async () => {
    const platform = window.navigator.platform;
    const resizeObserver = globalThis.ResizeObserver;
    Object.defineProperty(window.navigator, "platform", { configurable: true, value: "Win32" });
    globalThis.ResizeObserver = class {
      observe() {}
      unobserve() {}
      disconnect() {}
    } as unknown as typeof ResizeObserver;
    try {
      await renderStudio();
      fireEvent.click(screen.getByRole("button", { name: "Ausgabe-Einstellungen öffnen" }));
      await waitFor(() => expect(invokedCommands).toContainEqual({
        command: "set_preview_visible",
        args: { visible: false },
      }));

      fireEvent.keyDown(document, { key: "Escape" });
      await waitFor(() => expect(invokedCommands).toContainEqual({
        command: "set_preview_visible",
        args: { visible: true },
      }));

      const hiddenBeforeSourceDialog = invokedCommands.filter(({ command, args }) =>
        command === "set_preview_visible" && args?.visible === false
      ).length;
      fireEvent.click(screen.getByRole("button", { name: "Quelle hinzufügen" }));
      expect(screen.getByRole("dialog", { name: "Quelle hinzufügen" })).toBeTruthy();
      await waitFor(() => expect(invokedCommands.filter(({ command, args }) =>
        command === "set_preview_visible" && args?.visible === false
      )).toHaveLength(hiddenBeforeSourceDialog + 1));
    } finally {
      Object.defineProperty(window.navigator, "platform", { configurable: true, value: platform });
      if (resizeObserver) globalThis.ResizeObserver = resizeObserver;
      else delete (globalThis as { ResizeObserver?: typeof ResizeObserver }).ResizeObserver;
    }
  });

  it("maskiert einen Windows-Preview-Visibility-Fehler nicht durch erfolgreiche Bounds", async () => {
    const platform = window.navigator.platform;
    const resizeObserver = globalThis.ResizeObserver;
    Object.defineProperty(window.navigator, "platform", { configurable: true, value: "Win32" });
    globalThis.ResizeObserver = class {
      observe() {}
      unobserve() {}
      disconnect() {}
    } as unknown as typeof ResizeObserver;
    rejectedInvokeCommand = "set_preview_visible";
    try {
      await renderStudio();
      expect(await screen.findByText(/Vorschaufehler: Error: invoke failed/)).toBeTruthy();
    } finally {
      Object.defineProperty(window.navigator, "platform", { configurable: true, value: platform });
      if (resizeObserver) globalThis.ResizeObserver = resizeObserver;
      else delete (globalThis as { ResizeObserver?: typeof ResizeObserver }).ResizeObserver;
    }
  });

  it("wechselt die Szene über das Szenen-Dock per set_active_scene", async () => {
    await renderStudio();
    const target = project.scenes[project.scenes.length - 1]!;
    fireEvent.click(sceneRowButton(target.name));
    await waitFor(() => {
      expect(dispatchedCommands).toContainEqual(expect.objectContaining({ type: "set_active_scene", sceneId: target.id }));
    });
    expect(dispatchedCommands.filter((command) => command.type === "set_active_scene")).toHaveLength(1);
  });

  it("rollt eine neue Szene zurück, wenn ihre Aktivierung fehlschlägt", async () => {
    await renderStudio();
    rejectedDispatchType = "set_active_scene";
    fireEvent.click(screen.getByRole("button", { name: "Szene hinzufügen" }));

    await screen.findByText(/dispatch failed/);
    const added = dispatchedCommands.find((command) => command.type === "add_scene");
    expect(added).toBeTruthy();
    expect(dispatchedCommands).toContainEqual({ type: "set_active_scene", sceneId: added!.sceneId });
    expect(dispatchedCommands).toContainEqual({ type: "remove_scene", sceneId: added!.sceneId });
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
      expect(screen.getAllByText("Nicht in Szene").length).toBe(unplaced.length);
    }
  });

  it("bietet Szenenumbenennung als sichtbare Tastaturaktion an", async () => {
    await renderStudio();
    const scene = project.scenes[0]!;
    fireEvent.click(screen.getByRole("button", { name: `Szene „${scene.name}“ umbenennen` }));
    const input = screen.getByRole("textbox", { name: `Szene „${scene.name}“ umbenennen` });
    fireEvent.change(input, { target: { value: "Neue Szene" } });
    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() => expect(dispatchedCommands).toContainEqual({
      type: "rename_scene",
      sceneId: scene.id,
      name: "Neue Szene",
    }));
  });

  it("armiert die Quellen-Entfernung und entschärft bei pointerdown außerhalb", async () => {
    await renderStudio();
    fireEvent.click(placedSourceRowButton());
    const removeButton = screen.getByTitle("Ausgewählte Quelle entfernen");
    fireEvent.click(removeButton);
    expect(screen.getByTitle("Erneut klicken zum Entfernen")).toBeTruthy();
    fireEvent.pointerDown(document.body);
    expect(screen.getByTitle("Ausgewählte Quelle entfernen")).toBeTruthy();
    expect(dispatchedCommands.filter((command) => command.type === "remove_source")).toHaveLength(0);
  });

  it("entschärft die armierte Quellen-Entfernung bei Escape", async () => {
    await renderStudio();
    fireEvent.click(placedSourceRowButton());
    const removeButton = screen.getByTitle("Ausgewählte Quelle entfernen");
    fireEvent.click(removeButton);
    expect(screen.getByTitle("Erneut klicken zum Entfernen")).toBeTruthy();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.getByTitle("Ausgewählte Quelle entfernen")).toBeTruthy();
    expect(dispatchedCommands.filter((command) => command.type === "remove_source")).toHaveLength(0);
  });

  it("bestätigt die Quellen-Entfernung bei schnellen Mehrfachklicks genau einmal", async () => {
    await renderStudio();
    const source = placedSource();
    fireEvent.click(placedSourceRowButton());
    const removeButton = screen.getByTitle("Ausgewählte Quelle entfernen");
    fireEvent.click(removeButton);
    fireEvent.click(removeButton);
    fireEvent.click(removeButton);
    fireEvent.click(removeButton);
    await waitFor(() => {
      expect(dispatchedCommands.filter((command) => command.type === "remove_source")).toHaveLength(1);
    });
    expect(dispatchedCommands.find((command) => command.type === "remove_source")).toEqual(
      expect.objectContaining({ type: "remove_source", sourceId: source.id }),
    );
  });

  it("behält die Quellenauswahl bei fehlgeschlagener Entfernung", async () => {
    await renderStudio();
    rejectedDispatchType = "remove_source";
    const sourceButton = placedSourceRowButton();
    fireEvent.click(sourceButton);
    const removeButton = screen.getByTitle("Ausgewählte Quelle entfernen");
    fireEvent.click(removeButton);
    fireEvent.click(removeButton);

    await screen.findByText(/dispatch failed/);
    expect(sourceButton.closest(".source-row")?.classList.contains("selected")).toBe(true);
  });

  it("erlaubt das Entfernen der letzten Szene nicht", async () => {
    // Der Store ist ein Modul-Singleton und hat den Contract-Fixture bereits
    // geladen (eine Szene): genau dann muss die Entfernung blockiert sein.
    await renderStudio();
    const removeButton = screen.getByTitle("Aktive Szene entfernen") as HTMLButtonElement;
    expect(removeButton.disabled).toBe(true);
    fireEvent.click(removeButton);
    fireEvent.click(removeButton);
    fireEvent.click(removeButton);
    fireEvent.click(removeButton);
    await Promise.resolve();
    expect(dispatchedCommands.filter((command) => command.type === "remove_scene")).toHaveLength(0);
  });
});
