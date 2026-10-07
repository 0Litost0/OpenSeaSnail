import type { paths, components } from "./openapi";
import { request } from "./transport";
import { unwrap } from "./errors";

type ListModelsOperation = paths["/models"]["get"];
export type ModelList = ListModelsOperation["responses"][200]["content"]["application/json"];
export type Model = ModelList[number];
export type ComponentDto = components["schemas"]["Component"];

/** `GET /models` — 列模型及可选组件状态（funasr 条目带 `components`）。 */
export async function listModels(): Promise<ModelList> {
  return unwrap(request<ModelList, "/models">({ method: "GET", path: "/models" }));
}

/** 开发调试用：显式激活已安装的本地 runtime。release UI 不暴露此操作。 */
export async function activateModel(id: string): Promise<Model> {
  return unwrap(
    request<Model, "/models/{id}">({
      method: "POST",
      path: `/models/${encodeURIComponent(id)}` as "/models/{id}",
      body: { action: "activate" },
    }),
  );
}

type PutComponentOperation = paths["/models/{id}"]["put"];
export type ComponentEnableInput = PutComponentOperation["requestBody"]["content"]["application/json"];

/** `PUT /models/{component} {enabled}` — 开/关可选组件（懒：仅持久化，下条转写才加载/卸载）。
 *  返回 202 无 body（经原生代理 body 为 null）；未下载即开启→422（按 `unprocessable_entity`
 *  稳定码本地化，不透传 daemon 消息）。 */
export async function putComponent(component: string, enabled: boolean): Promise<void> {
  await unwrap(
    request<unknown, "/models/{id}">({
      method: "PUT",
      path: `/models/${encodeURIComponent(component)}` as "/models/{id}",
      body: { enabled } satisfies ComponentEnableInput,
    }),
  );
}

/** `POST /models/{component}/download` — 显式触发后台按需下载（M4：punc）。返回 202 无 body；
 *  进度经 GET /models 的 Component.download_progress 轮询；422（后置/未下载即开）按稳定码本地化。 */
export async function downloadModel(component: string): Promise<void> {
  await unwrap(
    request<unknown, "/models/{id}/download">({
      method: "POST",
      path: `/models/${encodeURIComponent(component)}/download` as "/models/{id}/download",
    }),
  );
}
