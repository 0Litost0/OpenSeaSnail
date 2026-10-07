import { describe, expect, it } from "vitest";
import { realtimeTaskFixtures } from "./realtimeTaskFixtures";

describe("realtime task fixtures", () => {
  it("covers every native phase without sensitive payloads", () => {
    expect(Object.keys(realtimeTaskFixtures)).toEqual(["idle", "preparing", "recording", "submitting", "transcribing", "cleaning_up", "auto_pasting", "completed", "failed"]);
    for (const fixture of Object.values(realtimeTaskFixtures)) {
      expect(fixture).not.toHaveProperty("text");
      expect(fixture).not.toHaveProperty("audio");
      expect(fixture).not.toHaveProperty("token");
    }
  });
});
