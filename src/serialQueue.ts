/**
 * Serializes asynchronous work per key while allowing unrelated keys to run
 * concurrently. A failed operation never poisons the following queue entry.
 */
export class SerialQueue {
  private readonly tails = new Map<string, Promise<void>>();

  enqueue<T>(key: string, operation: () => Promise<T>): Promise<T> {
    const previous = this.tails.get(key) ?? Promise.resolve();
    const result = previous.then(operation, operation);
    const tail = result.then(
      () => undefined,
      () => undefined,
    );
    this.tails.set(key, tail);
    void tail.then(() => {
      if (this.tails.get(key) === tail) this.tails.delete(key);
    });
    return result;
  }
}
