import type { paths } from "./openapi";
import { request } from "./transport";
import { unwrap } from "./errors";

type JsonResponse<Operation, Status extends number> = Operation extends { responses: infer Responses }
  ? Responses extends Record<Status, { content: { "application/json": infer Body } }> ? Body : never : never;
type TokensOperation = paths["/tokens"];
type TokenCreated = JsonResponse<TokensOperation["post"], 201>;
export type Token = JsonResponse<TokensOperation["get"], 200>[number];
export type TokenCreateInput = TokensOperation["post"]["requestBody"]["content"]["application/json"];

/** 失败按 daemon `error.code` 或 HTTP 状态回退为稳定码，UI 经 `localizedError` 本地化。 */
export const listTokens = () => unwrap(request<Token[], "/tokens">({ method: "GET", path: "/tokens" }));
export const createToken = (body: TokenCreateInput) => unwrap(request<TokenCreated, "/tokens">({ method: "POST", path: "/tokens", body }));
export const revokeToken = (id: string) => unwrap(request<undefined, "/tokens/{id}">({ method: "DELETE", path: `/tokens/${encodeURIComponent(id)}` as "/tokens/{id}" }));
