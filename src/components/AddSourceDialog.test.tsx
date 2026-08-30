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
});
