import { describe, expect, it } from "vitest";
import type { SceneItem, Source, Transform } from "../types";
import { sourceRowsFor } from "./SourcesPanel";

const transform: Transform = {
  x: 0,
  y: 0,
  width: 1280,
  height: 720,
  rotationDegrees: 0,
  cropTop: 0,
  cropRight: 0,
  cropBottom: 0,
  cropLeft: 0,
  opacity: 1,
};

describe("sourceRowsFor", () => {
  it("shows top layers first, then unplaced sources, with valid movement boundaries", () => {
    const sources: Source[] = [
      { type: "image", id: "bottom", name: "Unten", path: "/bottom.png" },
      { type: "image", id: "top", name: "Oben", path: "/top.png" },
      { type: "image", id: "global", name: "Global", path: "/global.png" },
    ];
    const items: SceneItem[] = [
      { id: "bottom-item", sourceId: "bottom", visible: true, locked: false, transform },
      { id: "top-item", sourceId: "top", visible: true, locked: true, transform },
    ];

    const rows = sourceRowsFor(sources, items);

    expect(rows.map((row) => row.source.id)).toEqual(["top", "bottom", "global"]);
    expect(rows[0]).toMatchObject({ locked: true, canMoveUp: false, canMoveDown: false });
    expect(rows[1]).toMatchObject({ locked: false, canMoveUp: true, canMoveDown: false });
    expect(rows[2]).not.toHaveProperty("itemId");
    expect(rows[2]).not.toHaveProperty("canMoveUp");
    expect(rows[2]).not.toHaveProperty("canMoveDown");
  });
});
