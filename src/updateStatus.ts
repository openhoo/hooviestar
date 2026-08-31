export type UpdateStatusEvent =
  | { status: "checking" }
  | { status: "up_to_date" }
  | { status: "available"; version: string }
  | { status: "downloading"; version: string; progress: number | null }
  | { status: "installing"; version: string }
  | { status: "installed"; version: string }
  | { status: "error"; message: string };

export function updateStatusMessage(event: UpdateStatusEvent): string {
  switch (event.status) {
    case "checking":
      return "Suche nach Aktualisierungen …";
    case "up_to_date":
      return "Hooviestar ist aktuell";
    case "available":
      return `Aktualisierung ${event.version} gefunden`;
    case "downloading":
      return event.progress === null
        ? `Aktualisierung ${event.version} wird heruntergeladen …`
        : `Aktualisierung ${event.version} wird heruntergeladen (${event.progress} %)`;
    case "installing":
      return `Aktualisierung ${event.version} wird installiert …`;
    case "installed":
      return `Aktualisierung ${event.version} installiert; Neustart …`;
    case "error":
      return `Aktualisierung fehlgeschlagen: ${event.message}`;
  }
}
