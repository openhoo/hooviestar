/**
 * Fängt Rejektionen feuergelassener Async-Flows ab und macht sie über den
 * übergebenen Meldungskanal sichtbar; der Kanal wird vor dem Start geleert,
 * damit eine erneute Ausführung die alte Meldung verwirft.
 */
export async function runGuarded(
  flow: () => Promise<unknown>,
  onError: (message: string | null) => void,
): Promise<void> {
  onError(null);
  try {
    await flow();
  } catch (error) {
    onError(String(error));
  }
}
