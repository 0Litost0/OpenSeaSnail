import type { paths } from "./openapi";
import { request } from "./transport";
import { unwrap } from "./errors";

type ListSessionsOperation = paths["/sessions"]["get"];
export type ListSessionsOptions = NonNullable<ListSessionsOperation["parameters"]["query"]>;
export type SessionList = ListSessionsOperation["responses"][200]["content"]["application/json"];
export type SessionSource = ListSessionsOptions["source"];
type SessionOperation = paths["/sessions/{id}"]["get"];
export type Session = SessionOperation["responses"][200]["content"]["application/json"];
type SearchOperation = paths["/sessions/search"]["get"];
export type SearchSessionsOptions = SearchOperation["parameters"]["query"];

/** OpenAPI 的 /sessions 游标直接透传给受限的原生代理；失败按稳定码回退。 */
export async function listSessions(options: ListSessionsOptions = {}): Promise<SessionList> {
  return unwrap(
    request<SessionList, "/sessions">({
      method: "GET",
      path: "/sessions",
      query: {
        cursor: options.cursor,
        limit: options.limit?.toString(),
        source: options.source,
      },
    }),
  );
}

/** 仅在转译尚未结束时轮询；完成或失败后立即停止。 */
export async function getSession(id: string): Promise<Session> {
  return unwrap(
    request<Session, "/sessions/{id}">({
      method: "GET",
      path: `/sessions/${encodeURIComponent(id)}` as "/sessions/{id}",
    }),
  );
}

export async function searchSessions(options: SearchSessionsOptions): Promise<SessionList> {
  return unwrap(
    request<SessionList, "/sessions/search">({
      method: "GET",
      path: "/sessions/search",
      query: { q: options.q, cursor: options.cursor, limit: options.limit?.toString(), source: options.source },
    }),
  );
}
