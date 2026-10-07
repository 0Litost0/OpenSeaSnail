import { describe, expect, it } from "vitest";
import { contextThumbnailQueryKey } from "./queries";

describe("session query keys", () => {
  it("isolates thumbnail data by account epoch", () => {
    expect(contextThumbnailQueryKey(1, "same-id", 2, 3)).not.toEqual(
      contextThumbnailQueryKey(2, "same-id", 2, 3),
    );
  });
});
