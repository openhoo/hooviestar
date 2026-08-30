import { describe, expect, it } from "vitest";
import { updateStatusMessage } from "./updateStatus";

describe("update status", () => {
  it("renders lifecycle stages", () => {
    expect(updateStatusMessage({ status: "checking" })).toContain("Suche");
    expect(updateStatusMessage({ status: "up_to_date" })).toContain("aktuell");
    expect(updateStatusMessage({ status: "available", version: "1.2.3" })).toContain("1.2.3");
    expect(updateStatusMessage({ status: "downloading", version: "1.2.3" })).toContain("installiert");
    expect(updateStatusMessage({ status: "installed", version: "1.2.3" })).toContain("Neustart");
  });

  it("keeps updater errors visible", () => {
    expect(updateStatusMessage({ status: "error", message: "offline" })).toBe(
      "Aktualisierung fehlgeschlagen: offline",
    );
  });
});
