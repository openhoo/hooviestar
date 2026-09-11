/**
 * Serializes asynchronous work per key while allowing unrelated keys to run
 * concurrently. A failed operation never poisons the following queue entry.
 */
export class SerialQueue {
  private readonly tails = new Map<string, Promise<void>>();
  // Keep each operation's actual result until it settles. The barrier must
  // observe a rejected entry even when a later entry on the same key is
  // allowed to continue the queue.
  private readonly pending = new Set<Promise<unknown>>();

  enqueue<T>(key: string, operation: () => Promise<T>): Promise<T> {
    const previous = this.tails.get(key) ?? Promise.resolve();
    const result = previous.then(operation, operation);
    const tail = result.then(
      () => undefined,
      () => undefined,
    );
    this.tails.set(key, tail);
    this.pending.add(result);
    void result.then(
      () => {
        this.pending.delete(result);
        if (this.tails.get(key) === tail) this.tails.delete(key);
      },
      () => {
        this.pending.delete(result);
        if (this.tails.get(key) === tail) this.tails.delete(key);
      },
    );
    return result;
  }

  /**
   * Waits for every source mutation currently enqueued. Rejections remain
   * observable for this barrier while the queue itself still recovers for
   * subsequent entries.
   */
  async waitForIdle(): Promise<void> {
    while (this.pending.size > 0) {
      const pending = [...this.pending];
      const results = await Promise.allSettled(pending);
      const rejected = results.find(
        (result): result is PromiseRejectedResult => result.status === "rejected",
      );
      if (rejected) throw rejected.reason;
    }
  }
}
