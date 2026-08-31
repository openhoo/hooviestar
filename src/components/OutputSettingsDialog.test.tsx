// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { OutputConfig } from "../types";
import { OutputSettingsDialog } from "./OutputSettingsDialog";

const initial: OutputConfig = { width: 1280, height: 720, fps: 30, background: "#101418" };

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

describe("OutputSettingsDialog", () => {
  afterEach(cleanup);

  it("starts authoritative, supports independent quality choices, and submits one complete config", async () => {
    const onApply = vi.fn(async () => undefined);
    const onOpenChange = vi.fn();
    render(<OutputSettingsDialog open output={initial} onApply={onApply} onOpenChange={onOpenChange} />);

    expect(screen.getByRole("dialog", { name: "Ausgabe-Einstellungen" })).toBeTruthy();
    expect((screen.getByRole("button", { name: "Anwenden" }) as HTMLButtonElement).disabled).toBe(true);

    fireEvent.change(screen.getByLabelText("Auflösung"), { target: { value: "1920x1080" } });
    fireEvent.change(screen.getByLabelText("Bildrate"), { target: { value: "30" } });
    fireEvent.change(screen.getByLabelText("Hintergrundfarbe"), { target: { value: "#Aa5500" } });
    fireEvent.click(screen.getByRole("button", { name: "Anwenden" }));

    await waitFor(() => expect(onApply).toHaveBeenCalledWith({
      width: 1920,
      height: 1080,
      fps: 30,
      background: "#aa5500",
    }));
    expect(onApply).toHaveBeenCalledTimes(1);
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it("rejects malformed colors before dispatch", () => {
    const onApply = vi.fn(async () => undefined);
    render(<OutputSettingsDialog open output={initial} onApply={onApply} onOpenChange={vi.fn()} />);
    fireEvent.change(screen.getByLabelText("Hintergrundfarbe"), { target: { value: "#123" } });

    expect(screen.getByText(/sechsstelligen Hex-Wert/)).toBeTruthy();
    expect(screen.getByLabelText("Hintergrundfarbe").getAttribute("aria-invalid")).toBe("true");
    expect((screen.getByRole("button", { name: "Anwenden" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.submit(screen.getByRole("button", { name: "Anwenden" }).closest("form")!);
    expect(onApply).not.toHaveBeenCalled();
  });

  it("preserves an unsaved draft across equivalent project snapshots", () => {
    const onApply = vi.fn(async () => undefined);
    const onOpenChange = vi.fn();
    const view = render(
      <OutputSettingsDialog open output={initial} onApply={onApply} onOpenChange={onOpenChange} />,
    );
    fireEvent.change(screen.getByLabelText("Hintergrundfarbe"), { target: { value: "#334455" } });

    view.rerender(
      <OutputSettingsDialog open output={{ ...initial }} onApply={onApply} onOpenChange={onOpenChange} />,
    );

    expect((screen.getByLabelText("Hintergrundfarbe") as HTMLInputElement).value).toBe("#334455");
    expect((screen.getByRole("button", { name: "Anwenden" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("keeps the dialog open and editable after a failed engine command", async () => {
    const onApply = vi.fn(async () => { throw new Error("GPU resize failed"); });
    const onOpenChange = vi.fn();
    render(<OutputSettingsDialog open output={initial} onApply={onApply} onOpenChange={onOpenChange} />);
    fireEvent.change(screen.getByLabelText("Bildrate"), { target: { value: "60" } });
    fireEvent.click(screen.getByRole("button", { name: "Anwenden" }));

    expect((await screen.findByRole("alert")).textContent).toContain("GPU resize failed");
    expect(screen.getByRole("dialog", { name: "Ausgabe-Einstellungen" })).toBeTruthy();
    expect((screen.getByRole("button", { name: "Anwenden" }) as HTMLButtonElement).disabled).toBe(false);
    expect(onOpenChange).not.toHaveBeenCalledWith(false);
  });

  it("locks close and duplicate submit while an apply is pending", async () => {
    const pending = deferred<void>();
    const onApply = vi.fn(() => pending.promise);
    const onOpenChange = vi.fn();
    render(<OutputSettingsDialog open output={initial} onApply={onApply} onOpenChange={onOpenChange} />);
    fireEvent.change(screen.getByLabelText("Bildrate"), { target: { value: "60" } });
    fireEvent.click(screen.getByRole("button", { name: "Anwenden" }));

    expect((screen.getByRole("button", { name: "Wird angewendet …" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "Einstellungen schließen" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.keyDown(document, { key: "Escape" });
    fireEvent.submit(screen.getByRole("button", { name: "Wird angewendet …" }).closest("form")!);
    expect(onApply).toHaveBeenCalledTimes(1);
    expect(onOpenChange).not.toHaveBeenCalledWith(false);

    pending.resolve();
    await waitFor(() => expect(onOpenChange).toHaveBeenCalledWith(false));
  });
});
