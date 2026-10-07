import type { paths } from "./openapi";
import { request } from "./transport";
import { unwrap } from "./errors";

type JsonResponse<Operation, Status extends number> = Operation extends {
  responses: infer Responses;
}
  ? Responses extends Record<Status, { content: { "application/json": infer Body } }>
    ? Body
    : never
  : never;

type AuthStatusOperation = paths["/auth/status"]["get"];
type SetupOperation = paths["/auth/setup"]["post"];
type AccountsOperation = paths["/accounts"];
type UnlockOperation = paths["/accounts/{id}/unlock"]["post"];

export type AuthStatus = JsonResponse<AuthStatusOperation, 200>;
export type SetupInput = SetupOperation["requestBody"]["content"]["application/json"];
export type Account = JsonResponse<AccountsOperation["get"], 200>[number];
export type UnlockInput = UnlockOperation["requestBody"]["content"]["application/json"];

/** 失败按 daemon `error.code` 或 HTTP 状态回退为稳定码，UI 经 `localizedError` 本地化。 */
export const authStatus = () => unwrap(request<AuthStatus, "/auth/status">({ method: "GET", path: "/auth/status" }));
export const setupAccount = (body: SetupInput) => unwrap(request({ method: "POST", path: "/auth/setup", body }));
export const listAccounts = () => unwrap(request<Account[], "/accounts">({ method: "GET", path: "/accounts" }));
export const createAccount = (body: SetupInput) => unwrap(request({ method: "POST", path: "/accounts", body }));
export const unlockAccount = (id: string, body: UnlockInput) =>
  unwrap(request({ method: "POST", path: `/accounts/${encodeURIComponent(id)}/unlock` as "/accounts/{id}/unlock", body }));
export const deleteAccount = (id: string) =>
  unwrap(request<undefined, "/accounts/{id}">({ method: "DELETE", path: `/accounts/${encodeURIComponent(id)}` as "/accounts/{id}" }));
