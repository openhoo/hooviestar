import { beforeEach, describe, expect, it, vi } from "vitest";
import fixture from "../contracts/project-v1.json";

const mocks = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
}));

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: mocks.listen }));

describe("engineStore startup lifecycle", () => {
  beforeEach(() => {
    vi.resetModules();
    mocks.invoke.mockReset();
    mocks.listen.mockReset();
  });

  it("detaches a listener before retrying a failed readiness handshake", async () => {
    const firstUnlisten = vi.fn();
    const secondUnlisten = vi.fn();
    let readinessAttempts = 0;
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "get_snapshot") return structuredClone(fixture);
      if (command === "engine_status" && readinessAttempts++ === 0) {
        throw new Error("backend unavailable");
      }
      return "running";
    });
    mocks.listen
      .mockResolvedValueOnce(firstUnlisten)
      .mockResolvedValueOnce(secondUnlisten);

    const { engineStore } = await import("./engineStore");
    await engineStore.start();
    expect(firstUnlisten).toHaveBeenCalledOnce();
    expect(engineStore.getSnapshot().status).toContain("Engine-Status nicht bestätigt:");

    await engineStore.start();
    expect(mocks.listen).toHaveBeenCalledTimes(2);
    expect(secondUnlisten).not.toHaveBeenCalled();
    expect(engineStore.getSnapshot().status).toBe("Program bereit");

    // Erfolgreicher Start bleibt idempotent und hängt keinen dritten Listener an.
    await engineStore.start();
    expect(mocks.listen).toHaveBeenCalledTimes(2);
  });

  it("shares an in-flight startup attempt with concurrent callers", async () => {
    const snapshot = deferred<unknown>();
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "get_snapshot") return snapshot.promise;
      return "running";
    });
    mocks.listen.mockResolvedValue(vi.fn());

    const { engineStore } = await import("./engineStore");
    const first = engineStore.start();
    let secondFinished = false;
    const second = engineStore.start().then(() => { secondFinished = true; });
    await Promise.resolve();
    expect(secondFinished).toBe(false);
    expect(mocks.invoke).toHaveBeenCalledTimes(1);

    snapshot.resolve(structuredClone(fixture));
    await Promise.all([first, second]);
    expect(mocks.listen).toHaveBeenCalledOnce();
    expect(mocks.invoke.mock.calls.filter(([command]) => command === "engine_status")).toHaveLength(1);
  });

  it("keeps output authoritative until a valid snapshot event confirms dispatch", async () => {
    const engineListeners: Array<(event: { payload: unknown }) => void> = [];
    mocks.invoke.mockImplementation(async (command: string) => {
      if (command === "get_snapshot") return structuredClone(fixture);
      return null;
    });
    mocks.listen.mockImplementation(async (_event: string, listener: (event: { payload: unknown }) => void) => {
      engineListeners.push(listener);
      return vi.fn();
    });

    const { engineStore } = await import("./engineStore");
    await engineStore.start();
    const requested = { width: 1920, height: 1080, fps: 30, background: "#224466" };
    await engineStore.dispatch({ type: "set_output_config", output: requested });

    expect(engineStore.getSnapshot().project?.output).toEqual(fixture.output);
    expect(mocks.invoke).toHaveBeenCalledWith("dispatch", {
      command: { type: "set_output_config", output: requested },
    });

    const confirmed = structuredClone(fixture);
    confirmed.output = requested;
    expect(engineListeners).toHaveLength(1);
    engineListeners[0]!({
      payload: { type: "snapshot", project: confirmed },
    });
    expect(engineStore.getSnapshot().project?.output).toEqual(requested);
  });
});
