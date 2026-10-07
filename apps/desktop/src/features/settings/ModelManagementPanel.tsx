import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { activateModel, listModels, putComponent, downloadModel, type ComponentDto } from "@/api/models";
import { localizedError } from "@/api/errors";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Field, FieldContent, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { useI18n } from "@/i18n/I18nProvider";

/** 下载中：progress ∈ [0,1)。progress==0 也算下载中（M4：排队→首块前不丢轮询）。null=无下载。 */
function downloading(c: ComponentDto): boolean {
  return c.download_progress != null && c.download_progress < 1;
}

function runtimeLabel(runtime: string, t: ReturnType<typeof useI18n>["t"]): string {
  switch (runtime) {
    case "sherpa_onnx":
      return "SenseVoice (Sherpa ONNX)";
    case "gguf":
      return "SenseVoice (local)";
    case "funasr":
      return "FunASR (migration fallback)";
    case "whisper":
      return "Whisper (local)";
    default:
      return t("app.title");
  }
}

// 与 daemon 的 `debug-backend-switching` Cargo feature 由打包脚本同源传入。
// `vite build` 下 DEV 恒为 false，不能把它当作内部迁移包的产品开关。
const debugBackendSwitching = import.meta.env.VITE_DEBUG_BACKEND_SWITCHING === "true";

/** 模型组件管理页（M3.3）：列出 funasr 后端可选组件（punc/spk）的下载状态与开关。
 *  开关懒加载：仅持久化，下一条转写才加载/卸载。spk 后置（toggleable=false）标"敬请期待"。
 *  未下载组件显示"下载"按钮（M4：触发后台下载，进度经轮询展示）。 */
export function ModelManagementPanel() {
  const { t } = useI18n();
  const queryClient = useQueryClient();
  // 轮询 GET /models：仅当有组件在下载中时 1s 一拉（M4 下载期）；无下载则不轮询。
  const models = useQuery({
    queryKey: ["models"],
    queryFn: listModels,
    refetchInterval: (query) => {
      const comps = query.state.data?.find((m) => m.runtime === "funasr")?.components ?? [];
      return comps.some(downloading) ? 1000 : false;
    },
  });
  const toggle = useMutation({
    mutationFn: ({ component, enabled }: { component: string; enabled: boolean }) =>
      putComponent(component, enabled),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["models"] }),
  });
  const download = useMutation({
    mutationFn: ({ component }: { component: string }) => downloadModel(component),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["models"] }),
  });
  const activate = useMutation({
    mutationFn: activateModel,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["models"] }),
  });

  const components: ComponentDto[] =
    models.data?.find((m) => m.runtime === "funasr")?.components ?? [];
  const defaultModel = models.data?.find((m) => m.default);

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("model.title")}</CardTitle>
        <CardDescription>{t("model.description")}</CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        {defaultModel ? (
          <Alert>
            <AlertTitle>{t("model.default", { runtime: runtimeLabel(defaultModel.runtime, t) })}</AlertTitle>
            <AlertDescription>
              {defaultModel.status === "active" ? t("model.active") : defaultModel.status === "installed" ? t("model.installed") : t("model.not_ready")}
            </AlertDescription>
          </Alert>
        ) : null}
        {debugBackendSwitching ? (
          <Tabs defaultValue="components">
            <TabsList aria-label={t("model.settings")}>
              <TabsTrigger value="components">{t("model.components")}</TabsTrigger>
              <TabsTrigger value="debug">{t("model.debug")}</TabsTrigger>
            </TabsList>
            <TabsContent value="debug" className="pt-4">
              <Field>
                <FieldLabel>{t("model.debug.label")}</FieldLabel>
                <FieldDescription>{t("model.debug.description")}</FieldDescription>
                <ToggleGroup
                  type="single"
                  value={models.data?.find((model) => model.status === "active")?.id}
                  onValueChange={(id) => id && activate.mutate(id)}
                >
                  {models.data?.filter((model) => model.id === "sensevoice-small" || model.id === "funasr-default").map((model) => (
                    <ToggleGroupItem key={model.id} value={model.id} disabled={model.status === "not_installed" || activate.isPending}>
                      {runtimeLabel(model.runtime, t)}
                    </ToggleGroupItem>
                  ))}
                </ToggleGroup>
              </Field>
              {activate.error ? <Alert variant="destructive"><AlertTitle>{t("model.switch_failed")}</AlertTitle><AlertDescription>{localizedError(t, activate.error)}</AlertDescription></Alert> : null}
            </TabsContent>
            <TabsContent value="components" className="pt-4">
              <ComponentList pending={models.isPending} error={models.isError} components={components} defaultRuntime={defaultModel?.runtime} togglePending={toggle.isPending} downloadPending={download.isPending} toggleError={toggle.error} modelsError={models.error} onToggle={(component, enabled) => toggle.mutate({ component, enabled })} onDownload={(component) => download.mutate({ component })} />
            </TabsContent>
          </Tabs>
        ) : (
          <ComponentList pending={models.isPending} error={models.isError} components={components} defaultRuntime={defaultModel?.runtime} togglePending={toggle.isPending} downloadPending={download.isPending} toggleError={toggle.error} modelsError={models.error} onToggle={(component, enabled) => toggle.mutate({ component, enabled })} onDownload={(component) => download.mutate({ component })} />
        )}
      </CardContent>
    </Card>
  );
}

function ComponentList({ pending, error, components, defaultRuntime, togglePending, downloadPending, toggleError, modelsError, onToggle, onDownload }: { pending: boolean; error: boolean; components: ComponentDto[]; defaultRuntime?: string; togglePending: boolean; downloadPending: boolean; toggleError: Error | null; modelsError: Error | null; onToggle: (component: string, enabled: boolean) => void; onDownload: (component: string) => void }) {
  const { t } = useI18n();
  return <>
        {pending ? (
          <Skeleton className="h-16 w-full" />
        ) : error ? null : components.length === 0 ? (
          <Alert>
            <AlertTitle>{t("model.none.title")}</AlertTitle>
            <AlertDescription>
              {defaultRuntime === "gguf" || defaultRuntime === "sherpa_onnx"
                ? t("model.none.local")
                : t("model.none.funasr")}
            </AlertDescription>
          </Alert>
        ) : (
          components.map((c) => (
            <ComponentRow
              key={c.id}
              component={c}
              pending={togglePending}
              onToggle={onToggle}
              onDownload={onDownload}
              downloadPending={downloadPending}
            />
          ))
        )}
        {modelsError || toggleError ? (
          <Alert variant="destructive">
            <AlertTitle>{t("model.update_failed")}</AlertTitle>
            <AlertDescription>
              {localizedError(t, toggleError ?? modelsError)}
            </AlertDescription>
          </Alert>
        ) : null}
      </>;
}

function ComponentRow({
  component: c,
  pending,
  onToggle,
  onDownload,
  downloadPending,
}: {
  component: ComponentDto;
  pending: boolean;
  onToggle: (component: string, enabled: boolean) => void;
  onDownload: (component: string) => void;
  downloadPending: boolean;
}) {
  const { t } = useI18n();
  const label = c.id === "punc" ? t("model.punctuation") : c.id === "spk" ? t("model.speaker") : c.id;
  const desc =
    c.id === "punc"
      ? t("model.punctuation.desc")
      : t("model.speaker.desc");

  // 不可操作（spk 后置等）→ "敬请期待"。
  if (!c.toggleable) {
    return (
      <Field orientation="horizontal" data-disabled>
        <Checkbox id={`comp-${c.id}`} checked={c.enabled} disabled />
        <FieldContent>
          <FieldLabel htmlFor={`comp-${c.id}`}>
          {label} <Badge variant="secondary">{t("model.coming_soon")}</Badge>
          </FieldLabel>
          <FieldDescription>{desc} {t("model.deferred")}</FieldDescription>
        </FieldContent>
      </Field>
    );
  }

  const busy = pending || downloading(c);
  return (
    <Field orientation="horizontal" data-disabled={busy || undefined}>
      <Checkbox
        id={`comp-${c.id}`}
        checked={c.enabled}
        disabled={busy || !c.downloaded}
        onCheckedChange={(checked) => onToggle(c.id, checked === true)}
      />
      <FieldContent>
        <FieldLabel htmlFor={`comp-${c.id}`}>
          {label}{" "}
          {c.downloaded ? (
            <Badge variant="secondary">{t("model.downloaded")}</Badge>
          ) : downloading(c) ? (
            <Badge variant="secondary">{t("model.downloading", { progress: Math.round((c.download_progress ?? 0) * 100) })}</Badge>
          ) : (
            <Button
              variant="outline"
              size="sm"
              onClick={() => onDownload(c.id)}
              disabled={downloadPending}
            >
              {t("model.download")}
            </Button>
          )}
        </FieldLabel>
        <FieldDescription>
          {desc} {c.downloaded ? t("model.ready_toggle") : downloading(c) ? t("model.downloading", { progress: Math.round((c.download_progress ?? 0) * 100) }) : t("model.not_downloaded")}。
        </FieldDescription>
      </FieldContent>
    </Field>
  );
}
