import { describe, expect, it, vi } from "vitest";
import { SerialQueue } from "./serialQueue";

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => { resolve = done; });
  return { promise, resolve };
}

describe("SerialQueue", () => {
  it("preserves order for one key while allowing another key to proceed", async () => {
    const queue = new SerialQueue();
    const gate = deferred();
    const order: string[] = [];
    const first = queue.enqueue("source-a", async () => {
      order.push("a1-start");
      await gate.promise;
      order.push("a1-end");
    });
    const second = queue.enqueue("source-a", async () => { order.push("a2"); });
    const unrelated = queue.enqueue("source-b", async () => { order.push("b1"); });

    await unrelated;
    expect(order).toEqual(["a1-start", "b1"]);
    gate.resolve();
    await Promise.all([first, second]);
    expect(order).toEqual(["a1-start", "b1", "a1-end", "a2"]);
  });

  it("continues after a rejected operation", async () => {
    const queue = new SerialQueue();
    const afterFailure = vi.fn(async () => "ok");
    const failed = queue.enqueue("source", async () => { throw new Error("failed"); });
    const next = queue.enqueue("source", afterFailure);

    await expect(failed).rejects.toThrow("failed");
    await expect(next).resolves.toBe("ok");
    expect(afterFailure).toHaveBeenCalledOnce();
  });

  it("waits for every source and propagates a pending rejection", async () => {
    const queue = new SerialQueue();
    const gate = deferred();
    const delayed = queue.enqueue("source-a", async () => {
      await gate.promise;
      return "ok";
    });
    queue.enqueue("source-b", async () => {
      throw new Error("failed");
    });

    const idle = queue.waitForIdle();
    let settled = false;
    void idle.then(
      () => { settled = true; },
      () => { settled = true; },
    );
    await Promise.resolve();
    expect(settled).toBe(false);

    gate.resolve();
    await expect(idle).rejects.toThrow("failed");
    await expect(delayed).resolves.toBe("ok");
  });
});
