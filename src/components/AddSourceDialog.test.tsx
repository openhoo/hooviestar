// @vitest-environment jsdom
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { AddSourceDialog } from "./AddSourceDialog";

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => { resolve = done; });
  return { promise, resolve };
}

describe("AddSourceDialog", () => {
  it("announces a failed runtime discovery as an error", async () => {
    render(
      <AddSourceDialog
        onAddText={vi.fn(async () => undefined)}
        onAddImage={vi.fn(async () => undefined)}
        onAddMedia={vi.fn(async () => undefined)}
        onAddCandidate={vi.fn(async () => undefined)}
        onEnumerate={vi.fn(async () => { throw new Error("runtime offline"); })}
        onSelectPortal={vi.fn(async () => ({
          candidates: [],
          portalSelectionRequired: false,
          message: null,
        }))}
        onClose={vi.fn()}
      />,
    );

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("runtime offline");
    expect(alert.classList.contains("source-message")).toBe(true);
  });

  it("keeps local content available and focused while runtime discovery is pending", async () => {
    let finishDiscovery!: (result: {
      candidates: [];
      portalSelectionRequired: false;
      message: null;
    }) => void;
    const discovery = new Promise<{
      candidates: [];
      portalSelectionRequired: false;
      message: null;
    }>((resolve) => { finishDiscovery = resolve; });

    render(
      <AddSourceDialog
        onAddText={vi.fn(async () => undefined)}
        onAddImage={vi.fn(async () => undefined)}
        onAddMedia={vi.fn(async () => undefined)}
        onAddCandidate={vi.fn(async () => undefined)}
        onEnumerate={vi.fn(() => discovery)}
        onSelectPortal={vi.fn(async () => ({
          candidates: [],
          portalSelectionRequired: false,
          message: null,
        }))}
        onClose={vi.fn()}
      />,
    );

    const text = screen.getByRole("button", { name: /Text/ }) as HTMLButtonElement;
    expect(text.disabled).toBe(false);
    expect(screen.getByText("Laufende Quellen werden gesucht …")).toBeTruthy();
    await waitFor(() => expect(document.activeElement).toBe(text));

    finishDiscovery({ candidates: [], portalSelectionRequired: false, message: null });
    await waitFor(() => expect(screen.queryByText("Laufende Quellen werden gesucht …")).toBeNull());
  });

  it("runs one add operation despite rapid repeated clicks", async () => {
    const pending = deferred();
    const onAddText = vi.fn(() => pending.promise);
    render(
      <AddSourceDialog
        onAddText={onAddText}
        onAddImage={vi.fn(async () => undefined)}
        onAddMedia={vi.fn(async () => undefined)}
        onAddCandidate={vi.fn(async () => undefined)}
        onEnumerate={vi.fn(async () => ({
          candidates: [],
          portalSelectionRequired: false,
          message: null,
        }))}
        onSelectPortal={vi.fn(async () => ({
          candidates: [],
          portalSelectionRequired: false,
          message: null,
        }))}
        onClose={vi.fn()}
      />,
    );

    const text = screen.getByRole("button", { name: /Text/ }) as HTMLButtonElement;
    await waitFor(() => expect(text.disabled).toBe(false));
    fireEvent.click(text);
    fireEvent.click(text);
    fireEvent.click(text);
    expect(onAddText).toHaveBeenCalledOnce();
    expect(text.disabled).toBe(true);

    pending.resolve();
    await waitFor(() => expect(text.disabled).toBe(false));
  });

  it("locks the dialog while the desktop portal owns selection", async () => {
    const selection = deferred();
    render(
      <AddSourceDialog
        onAddText={vi.fn(async () => undefined)}
        onAddImage={vi.fn(async () => undefined)}
        onAddMedia={vi.fn(async () => undefined)}
        onAddCandidate={vi.fn(async () => undefined)}
        onEnumerate={vi.fn(async () => ({
          candidates: [],
          portalSelectionRequired: true,
          message: "Fenster und Monitore werden über das Desktop-Portal ausgewählt.",
        }))}
        onSelectPortal={vi.fn(async () => {
          await selection.promise;
          return { candidates: [], portalSelectionRequired: false, message: null };
        })}
        onClose={vi.fn()}
      />,
    );

    const portal = await screen.findByRole("button", { name: /Fenster oder Monitor auswählen/ });
    fireEvent.click(portal);

    expect(screen.getByText("Auswahl wird geöffnet …")).toBeTruthy();
    expect((screen.getByRole("button", { name: /Text/ }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "Abbrechen" }) as HTMLButtonElement).disabled).toBe(true);

    selection.resolve();
    await waitFor(() => expect(screen.queryByText("Auswahl wird geöffnet …")).toBeNull());
  });
});
