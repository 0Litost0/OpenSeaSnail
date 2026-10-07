import { http, HttpResponse } from "msw";
import { setupServer } from "msw/node";

export const server = setupServer(
  http.get("http://127.0.0.1:43123/api/v1/sessions", () =>
    HttpResponse.json({ items: [], next_cursor: null }),
  ),
  http.get("http://127.0.0.1:43123/api/v1/dictionary", () =>
    HttpResponse.json({ items: [], next_cursor: null }),
  ),
);
