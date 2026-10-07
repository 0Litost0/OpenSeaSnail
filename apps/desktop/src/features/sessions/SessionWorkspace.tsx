import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ChevronDownIcon, FileIcon, ImageIcon, SearchIcon } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { SessionSource } from "@/api/sessions";
import { errorCode, localizedError } from "@/api/errors";
import { getContextThumbnail, copyText, openContextLink, openContextResource, type ContextResource, type CleanupDetail, type ContextItem, type TimelineItem, type WorkspaceDetail } from "@/api/transport";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "@/components/ui/empty";
import { Field, FieldDescription, FieldGroup, FieldLabel, FieldLegend, FieldSet } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import { Spinner } from "@/components/ui/spinner";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Textarea } from "@/components/ui/textarea";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { useI18n } from "@/i18n/I18nProvider";
import { useAccountEpoch } from "@/features/accounts/account-context";
import { cn } from "@/lib/utils";
import { resolveWideLayout, useElementWidth } from "@/hooks/use-element-width";
import { SessionPreview } from "./SessionPreview";
import { contextThumbnailQueryKey, useSession, useSessionCleanupDetail, useSessionSearch, useSessions, useSessionWorkspaceDetail } from "./queries";

export function shouldOpenContextLink(metaKey: boolean, clickDetail: number) {
  return metaKey || clickDetail === 0;
}

function ContextThumbnail({ sessionId, sequence, resource }: { sessionId: string; sequence: number; resource: ContextResource }) {
  const { t } = useI18n();
  const { epoch } = useAccountEpoch();
  const ref = useRef<HTMLSpanElement>(null);
  const [visible, setVisible] = useState(false);
  useEffect(() => {
    if (!ref.current || typeof IntersectionObserver === "undefined") { setVisible(true); return; }
    const observer = new IntersectionObserver(([entry]) => {
      if (entry.isIntersecting) { setVisible(true); observer.disconnect(); }
    });
    observer.observe(ref.current);
    return () => observer.disconnect();
  }, []);
  const thumbnail = useQuery({
    queryKey: contextThumbnailQueryKey(epoch, sessionId, sequence, resource.index),
    queryFn: () => getContextThumbnail(sessionId, sequence, resource.index),
    enabled: visible && resource.available,
    retry: (failureCount, error) => errorCode(error) === "thumbnail_busy" && failureCount < 2,
    retryDelay: (attempt) => Math.min(250 * 2 ** attempt, 1000),
    gcTime: 0,
  });
  return <span ref={ref} className="flex min-w-0 flex-col items-start gap-2">
    {thumbnail.data ? <img src={`data:${thumbnail.data.mime_type};base64,${thumbnail.data.base64}`} alt={resource.display_name} className="max-h-48 max-w-full object-contain" />
      : <span className="flex items-center gap-2"><ImageIcon /><span>{resource.display_name}</span></span>}
    {thumbnail.isError && <span className="text-sm text-muted-foreground">{localizedError(t, thumbnail.error)}</span>}
  </span>;
}

export function ContextItem({ sessionId, item }: { sessionId: string; item: ContextItem }) {
  const { t } = useI18n();
  const [error, setError] = useState<string>();
  const [expanded, setExpanded] = useState(false);
  const report = (reason: unknown) => setError(localizedError(t, reason));
  const errorText = error;

  if (item.kind === "context_file" || item.kind === "context_image") {
    const isImage = item.kind === "context_image";
    return (
      <TooltipProvider>
        <Card className="my-2 border-dashed">
          <CardContent className="flex flex-col gap-3 pt-4">
          {item.resources.map((resource) => (
            <Tooltip key={resource.index}>
              <TooltipTrigger asChild>
                <Button variant="outline" className="h-auto justify-start" disabled={!resource.available} aria-label={resource.path} onClick={() => void openContextResource(sessionId, item.sequence, resource.index).catch(report)}>
                  {isImage ? <ContextThumbnail sessionId={sessionId} sequence={item.sequence} resource={resource} /> : <span className="flex min-w-0 items-center gap-2 text-left"><FileIcon className="shrink-0" /><span className="whitespace-normal break-all">{resource.path}</span></span>}
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t(isImage ? "sessions.resource.open_image" : "sessions.resource.open_file")}</TooltipContent>
            </Tooltip>
          ))}
          {errorText && <p role="alert" className="text-sm text-destructive">{errorText}</p>}
          </CardContent>
        </Card>
      </TooltipProvider>
    );
  }

  // context_text | context_rich_text | context_link：原位弱化引用。
  const isRich = item.kind === "context_rich_text";
  const plain = item.kind === "context_rich_text" ? item.plain_text : item.kind === "context_link" ? item.url : item.text;
  const long = plain.length > 120 || plain.includes("\n") || isRich;
  const content = item.kind === "context_link" ? (
    <TooltipProvider><Tooltip><TooltipTrigger asChild><button className="break-all text-left underline decoration-dashed underline-offset-4" onClick={(event) => { event.preventDefault(); if (shouldOpenContextLink(event.metaKey, event.detail)) void openContextLink(sessionId, item.sequence).catch(report); }}>{item.url}</button></TooltipTrigger><TooltipContent>{t("sessions.resource.open_link")}</TooltipContent></Tooltip></TooltipProvider>
  ) : item.kind === "context_rich_text" ? (
    item.sanitized_html ? <div className="prose prose-sm max-w-none break-words" dangerouslySetInnerHTML={{ __html: item.sanitized_html }} /> : <span className="whitespace-pre-wrap break-words">{item.plain_text}</span>
  ) : (
    <span className="whitespace-pre-wrap break-words">{item.text}</span>
  );
  return (
    <aside className="my-1 rounded-md border-l-2 border-muted-foreground/30 bg-muted/60 px-2 py-1 text-sm text-muted-foreground" aria-label={t("sessions.context.label")}>
      {long ? <Collapsible open={expanded} onOpenChange={setExpanded}><div className={cn("pt-2", !expanded && "line-clamp-6")}>{content}</div><CollapsibleTrigger asChild><Button variant="ghost" size="sm"><ChevronDownIcon data-icon="inline-start" />{expanded ? t("sessions.context.collapse") : t("sessions.context.expand")}</Button></CollapsibleTrigger></Collapsible> : content}
      {errorText && <p role="alert" className="text-destructive">{errorText}</p>}
    </aside>
  );
}

/** 将工作台所见时间线合成为最终复制文本；视觉标记和 UI 提示不会进入结果。 */
export function serializeTimelineForCopy(items: TimelineItem[]): string {
  let output = "";
  for (const item of items) {
    if (item.kind === "transcript") {
      output += item.text;
      continue;
    }
    const value = item.kind === "final_text" ? item.text
      : item.kind === "context_text" ? item.text
      : item.kind === "context_rich_text" ? item.plain_text
      : item.kind === "context_link" ? item.url
      : item.resources.map((resource) => resource.path).join("\n");
    if (!value) continue;
    if (output && !output.endsWith("\n")) output += "\n";
    output += value;
    output += "\n";
  }
  return output.replace(/\n+$/, "");
}

export function QueryError({ retry }: { retry: () => void }) {
  const { t } = useI18n();
  return <Empty><EmptyHeader><EmptyTitle>{t("error.generic")}</EmptyTitle><EmptyDescription><Button variant="outline" onClick={retry}>{t("common.retry")}</Button></EmptyDescription></EmptyHeader></Empty>;
}

function TranscriptDetail({ sessionId, detail }: { sessionId?: string; detail?: WorkspaceDetail }) {
  const { t } = useI18n();
  const { epoch } = useAccountEpoch();
  const queryClient = useQueryClient();
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState<string>();
  const [processingOpen, setProcessingOpen] = useState(false);
  const processing = useSessionCleanupDetail(sessionId, processingOpen);
  useEffect(() => () => {
    if (sessionId) queryClient.removeQueries({ queryKey: ["session-cleanup-detail", epoch, sessionId], exact: true });
  }, [epoch, queryClient, sessionId]);
  if (!sessionId || !detail) return <Empty><EmptyHeader><EmptyTitle>{t("sessions.detail")}</EmptyTitle><EmptyDescription>{t("sessions.select")}</EmptyDescription></EmptyHeader></Empty>;
  return <div className="flex min-h-0 flex-col gap-3">
    {detail.context_degraded && <p role="status" className="text-sm text-muted-foreground">{t("sessions.context.degraded")}</p>}
    <div className="whitespace-pre-wrap break-words text-sm leading-7">{detail.display_items.map((item, index) => item.kind === "transcript" || item.kind === "final_text" ? <span key={index} data-speaker={item.kind === "transcript" && item.speaker ? item.speaker : undefined}>{item.kind === "transcript" && item.speaker && <Badge variant="outline" className="mx-1" aria-label={t("sessions.speaker").replace("{speaker}", item.speaker)}>{item.speaker}</Badge>}{item.text}</span> : <ContextItem key={`${item.sequence}-${index}`} sessionId={sessionId} item={item} />)}</div>
    {/* cleanup 会话在 presentation cache 缺失（如重启）后，context 以 separate 布局返回：
        daemon 不伪造原 inline 位置，这里在正文下方单独渲染，否则重启后上下文不可见。 */}
    {detail.separate_contexts.length > 0 && <div className="flex flex-col gap-1">
      <p className="text-xs font-medium text-muted-foreground">{t("sessions.context.label")}</p>
      <div className="whitespace-pre-wrap break-words text-sm leading-7">{detail.separate_contexts.map((item) => <ContextItem key={item.sequence} sessionId={sessionId} item={item} />)}</div>
    </div>}
    <Button variant="outline" className="self-start" onClick={() => {
      setCopyError(undefined);
      void copyText(serializeTimelineForCopy([...detail.display_items, ...detail.separate_contexts]))
        .then(() => { setCopied(true); window.setTimeout(() => setCopied(false), 1500); })
        .catch((reason) => setCopyError(localizedError(t, reason)));
    }}>{copied ? t("sessions.copy.success") : t("sessions.copy")}</Button>
    {copyError && <p role="alert" className="text-sm text-destructive">{copyError}</p>}
    <Collapsible open={processingOpen} onOpenChange={(open) => {
      setProcessingOpen(open);
      if (!open) queryClient.removeQueries({ queryKey: ["session-cleanup-detail", epoch, sessionId], exact: true });
    }}>
      <CollapsibleTrigger asChild>
        <Button variant="outline" className="self-start">
          <ChevronDownIcon data-icon="inline-start" />
          {processingOpen ? t("sessions.processing.hide") : t("sessions.processing.show")}
        </Button>
      </CollapsibleTrigger>
      <CollapsibleContent>{processing.isPending ? <div className="flex justify-center p-6"><Spinner /></div> : processing.isError ? <QueryError retry={() => void processing.refetch()} /> : processing.data ? <ProcessingDetails detail={processing.data} /> : null}</CollapsibleContent>
    </Collapsible>
  </div>;
}

function ProcessingDetails({ detail }: { detail: CleanupDetail }) {
  const { locale, t } = useI18n();
  const diagnostics = detail.diagnostics;
  const timestamp = (value: number | null) => {
    if (value === null || !Number.isFinite(value)) return t("sessions.processing.unavailable");
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) return t("sessions.processing.unavailable");
    return new Intl.DateTimeFormat(locale, { dateStyle: "medium", timeStyle: "medium" }).format(date);
  };
  const duration = (value: number | null | undefined) => {
    if (value === null || value === undefined || !Number.isFinite(value) || value < 0) return t("sessions.processing.unavailable");
    if (value < 1) return "< 1 ms";
    if (value < 1000) return `${Math.round(value)} ms`;
    return `${new Intl.NumberFormat(locale, { maximumFractionDigits: 2 }).format(value / 1000)} s`;
  };
  const responseCompletedAt = diagnostics?.response_completed_at_ms;
  const hasCompleteRequestTiming = diagnostics !== null
    && Number.isFinite(diagnostics.request_started_at_ms)
    && responseCompletedAt !== null
    && responseCompletedAt !== undefined
    && Number.isFinite(responseCompletedAt)
    && responseCompletedAt >= diagnostics.request_started_at_ms;
  const cleanupRequestElapsed = hasCompleteRequestTiming && diagnostics && responseCompletedAt !== null && responseCompletedAt !== undefined
    ? responseCompletedAt - diagnostics.request_started_at_ms
    : null;
  const localTranscriptionDuration = duration(diagnostics?.local_transcription_elapsed_ms);
  const cleanupRequestDuration = duration(cleanupRequestElapsed);
  const rawResponse = diagnostics?.raw_response
    ? diagnostics.raw_response
    : diagnostics?.raw_response_base64
      ? `${t("sessions.processing.base64")}\n${diagnostics.raw_response_base64}`
      : diagnostics
        ? t(`sessions.processing.capture_${diagnostics.capture_status}`)
        : t("sessions.processing.diagnostics_unavailable");
  return <Card size="sm" className="mt-3">
    <CardHeader><CardTitle>{t("sessions.processing.title")}</CardTitle><CardDescription>{t("sessions.processing.description")}</CardDescription></CardHeader>
    <CardContent>
      <FieldGroup>
        <FieldSet>
          <FieldLegend>{t("sessions.processing.content")}</FieldLegend>
          <FieldGroup>
            <Field><FieldLabel htmlFor="cleanup-cleaned-text">{t("sessions.processing.cleaned")}</FieldLabel><Textarea id="cleanup-cleaned-text" readOnly value={detail.cleaned_text ?? t("sessions.processing.unavailable")} className="max-h-64 resize-none overflow-auto" /></Field>
            <Field><FieldLabel htmlFor="cleanup-original-text">{t("sessions.processing.original")}</FieldLabel><Textarea id="cleanup-original-text" readOnly value={detail.original_text} className="max-h-64 resize-none overflow-auto" /></Field>
          </FieldGroup>
        </FieldSet>

        <FieldSet>
          <FieldLegend>{t("sessions.processing.corrections")}</FieldLegend>
          {detail.corrections.length === 0 ? <FieldDescription>{t("sessions.processing.no_corrections")}</FieldDescription> : <Table><TableHeader><TableRow><TableHead>{t("cleanup.prompt.original")}</TableHead><TableHead>{t("cleanup.prompt.corrected")}</TableHead><TableHead>{t("sessions.processing.kind")}</TableHead></TableRow></TableHeader><TableBody>{detail.corrections.map((correction, index) => <TableRow key={`${correction.original_text}-${correction.corrected_text}-${index}`}><TableCell className="whitespace-pre-wrap break-words">{correction.original_text}</TableCell><TableCell className="whitespace-pre-wrap break-words">{correction.corrected_text}</TableCell><TableCell><Badge variant="outline">{t(`cleanup.prompt.kind_${correction.kind}`)}</Badge></TableCell></TableRow>)}</TableBody></Table>}
        </FieldSet>

        <FieldSet>
          <FieldLegend>{t("sessions.processing.timeline")}</FieldLegend>
          <FieldDescription>{t("sessions.processing.timeline_description")}</FieldDescription>
          <div role="list" aria-label={t("sessions.processing.timeline")} className="grid grid-cols-[auto_minmax(1rem,1fr)_auto] items-center gap-2">
            <Badge role="listitem" aria-label={`${t("sessions.processing.phase_transcription")}: ${localTranscriptionDuration}`} variant="secondary">1</Badge><Separator aria-hidden="true" /><Badge role="listitem" aria-label={`${t("sessions.processing.phase_request")}: ${cleanupRequestDuration}`} variant="secondary">2</Badge>
          </div>
          <div className="grid grid-cols-2 gap-3 text-center text-sm">
            <div className="flex min-w-0 flex-col gap-1"><span className="font-medium">{t("sessions.processing.phase_transcription")}</span><span className="text-muted-foreground">{localTranscriptionDuration}</span></div>
            <div className="flex min-w-0 flex-col gap-1"><span className="font-medium">{t("sessions.processing.phase_request")}</span><span className="text-muted-foreground">{cleanupRequestDuration}</span></div>
          </div>
          <Field orientation="horizontal"><FieldLabel>{t("sessions.processing.total_cleanup")}</FieldLabel><Badge variant="outline">{duration(detail.cleanup_elapsed_ms)}</Badge></Field>
        </FieldSet>

        <FieldSet>
          <FieldLegend>{t("sessions.processing.response")}</FieldLegend>
          {!diagnostics && <FieldDescription>{t("sessions.processing.diagnostics_unavailable")}</FieldDescription>}
          <Field><FieldLabel htmlFor="cleanup-raw-response">{t("sessions.processing.raw_response")}</FieldLabel><Textarea id="cleanup-raw-response" readOnly value={rawResponse} className="max-h-80 resize-none overflow-auto" /></Field>
          {diagnostics && <FieldGroup className="grid sm:grid-cols-2">
            <Field><FieldLabel htmlFor="cleanup-trace-id">{t("sessions.processing.trace_id")}</FieldLabel><Input id="cleanup-trace-id" readOnly value={diagnostics.trace_id} /></Field>
            <Field><FieldLabel htmlFor="cleanup-http-status">{t("sessions.processing.status")}</FieldLabel><Input id="cleanup-http-status" readOnly value={`${diagnostics.http_status ?? "—"} · ${t(`sessions.processing.status_${diagnostics.capture_status}`)}`} /></Field>
            <Field><FieldLabel htmlFor="cleanup-request-started">{t("sessions.processing.request_started")}</FieldLabel><Input id="cleanup-request-started" readOnly value={timestamp(diagnostics.request_started_at_ms)} /></Field>
            <Field><FieldLabel htmlFor="cleanup-response-started">{t("sessions.processing.response_started")}</FieldLabel><Input id="cleanup-response-started" readOnly value={timestamp(diagnostics.response_started_at_ms)} /></Field>
            <Field><FieldLabel htmlFor="cleanup-response-completed">{t("sessions.processing.response_completed")}</FieldLabel><Input id="cleanup-response-completed" readOnly value={timestamp(diagnostics.response_completed_at_ms)} /></Field>
            <Field><FieldLabel htmlFor="cleanup-response-size">{t("sessions.processing.response_size")}</FieldLabel><Input id="cleanup-response-size" readOnly value={`${diagnostics.response_body_bytes} B`} /></Field>
            <Field><FieldLabel htmlFor="cleanup-provider-request-id">{t("sessions.processing.provider_request_id")}</FieldLabel><Input id="cleanup-provider-request-id" readOnly value={diagnostics.provider_request_id ?? t("sessions.processing.unavailable")} /></Field>
            <Field><FieldLabel htmlFor="cleanup-content-type">{t("sessions.processing.content_type")}</FieldLabel><Input id="cleanup-content-type" readOnly value={diagnostics.response_content_type ?? t("sessions.processing.unavailable")} /></Field>
            <Field className="sm:col-span-2"><FieldLabel htmlFor="cleanup-response-sha">{t("sessions.processing.response_sha")}</FieldLabel><Input id="cleanup-response-sha" readOnly value={diagnostics.response_sha256 || t("sessions.processing.unavailable")} /></Field>
          </FieldGroup>}
        </FieldSet>
      </FieldGroup>
    </CardContent>
  </Card>;
}

export function SessionWorkspace() {
  const { locale, t } = useI18n();
  const { ref, width } = useElementWidth<HTMLElement>();
  const wide = resolveWideLayout(width, false, 780);
  const [source, setSource] = useState<SessionSource>();
  const [term, setTerm] = useState("");
  const [selectedId, setSelectedId] = useState<string>();
  const lastTriggerRef = useRef<HTMLButtonElement>(null);
  const listing = useSessions(source);
  const searched = useSessionSearch(term, source);
  const session = useSession(selectedId);
  const detail = useSessionWorkspaceDetail(selectedId, session.data?.status === "completed");
  const active = term.trim() ? searched : listing;
  const items = active.data?.pages.flatMap((page) => page.items) ?? [];
  const detailContent = !selectedId ? <TranscriptDetail />
    : session.isError ? <QueryError retry={() => void session.refetch()} />
    : session.isPending || session.data?.status === "transcribing" ? <p role="status" className="text-sm text-muted-foreground">{t("sessions.transcribing")}</p>
    : session.data?.status === "failed" ? <p role="alert" className="text-sm text-destructive">{t("sessions.failed")}</p>
    : detail.isError ? <QueryError retry={() => void detail.refetch()} />
    : detail.isPending ? <Spinner />
    : <TranscriptDetail key={selectedId} sessionId={selectedId} detail={detail.data} />;

  const list = (
    <Card size="sm" className="flex min-h-0 flex-col">
      <CardHeader className="shrink-0"><CardTitle>{t("sessions.title")}</CardTitle><CardDescription>{t("sessions.description")}</CardDescription></CardHeader>
      <CardContent className="flex min-h-0 flex-1 flex-col gap-3">
        <FieldGroup className={cn("shrink-0 gap-3", wide && "flex-row items-end")}>
          <Field className="min-w-0 flex-1"><FieldLabel htmlFor="search">{t("sessions.search")}</FieldLabel><div className="flex gap-2"><Input id="search" value={term} onChange={(event) => setTerm(event.target.value)} placeholder={t("sessions.search.placeholder")} /><Button variant="outline" size="icon" aria-label={t("sessions.search")} onClick={() => void active.refetch()}><SearchIcon /></Button></div></Field>
          <Field className={cn(wide && "w-auto")}><FieldLabel>{t("sessions.source")}</FieldLabel><ToggleGroup type="single" value={source ?? "all"} onValueChange={(value) => setSource(value === "all" || !value ? undefined : value as SessionSource)} variant="outline" spacing={1}><ToggleGroupItem value="all">{t("sessions.source.all")}</ToggleGroupItem><ToggleGroupItem value="realtime">{t("sessions.source.realtime")}</ToggleGroupItem></ToggleGroup></Field>
        </FieldGroup>
        <ScrollArea className="min-h-0 flex-1">
          <Table>
            <TableHeader><TableRow><TableHead>{t("sessions.time")}</TableHead><TableHead>{t("sessions.source")}</TableHead><TableHead>{t("sessions.status")}</TableHead><TableHead>{t("sessions.preview")}</TableHead></TableRow></TableHeader>
            <TableBody>{items.map((session) => <TableRow key={session.id} data-state={selectedId === session.id ? "selected" : undefined}>
              <TableCell className="whitespace-nowrap">{new Intl.DateTimeFormat(locale, { dateStyle: "short", timeStyle: "short" }).format(new Date(session.created_at))}</TableCell>
              <TableCell><Badge variant="outline">{t(session.source === "realtime" ? "sessions.source.realtime" : "sessions.source.imported")}</Badge></TableCell>
              <TableCell><Badge variant={session.status === "failed" ? "destructive" : "secondary"}>{t(`sessions.status.${session.status}`)}</Badge></TableCell>
              <TableCell><Button variant="ghost" className="h-auto w-full min-w-0 justify-start px-0 text-left" aria-label={session.preview || t(`sessions.status.${session.status}`)} aria-describedby={`preview-context-${session.id}`} onClick={(event) => { lastTriggerRef.current = event.currentTarget; setSelectedId(session.id); }}><SessionPreview sessionId={session.id} preview={session.preview} status={session.status} /></Button></TableCell>
            </TableRow>)}</TableBody>
          </Table>
          {active.isPending && <div className="flex justify-center p-6"><Spinner /></div>}
          {active.isError && <QueryError retry={() => void active.refetch()} />}
          {!active.isPending && !active.isError && items.length === 0 && <Empty><EmptyHeader><EmptyTitle>{t("sessions.empty")}</EmptyTitle></EmptyHeader></Empty>}
        </ScrollArea>
        {active.hasNextPage && <Button variant="outline" disabled={active.isFetchingNextPage} onClick={() => void active.fetchNextPage()}>{active.isFetchingNextPage && <Spinner data-icon="inline-start" />}{t("sessions.loadMore")}</Button>}
      </CardContent>
    </Card>
  );

  return <section ref={ref} className={cn("grid h-full min-h-0 gap-3", wide && "grid-cols-[minmax(30rem,1.15fr)_minmax(18rem,0.85fr)]")}>{list}{wide ? <Card size="sm" className="flex min-h-0 flex-col"><CardHeader className="shrink-0"><CardTitle>{t("sessions.detail")}</CardTitle><CardDescription>{t("sessions.detail.description")}</CardDescription></CardHeader><CardContent className="min-h-0 flex-1"><ScrollArea className="h-full pr-4">{detailContent}</ScrollArea></CardContent></Card> : <Sheet open={Boolean(selectedId)} onOpenChange={(open) => { if (!open) setSelectedId(undefined); }}><SheetContent closeLabel={t("common.close")} className="w-full sm:max-w-xl" onCloseAutoFocus={(event) => { event.preventDefault(); lastTriggerRef.current?.focus(); }}><SheetHeader><SheetTitle>{t("sessions.detail")}</SheetTitle><SheetDescription>{t("sessions.detail.description")}</SheetDescription></SheetHeader><ScrollArea className="min-h-0 flex-1 px-4 pb-4">{detailContent}</ScrollArea></SheetContent></Sheet>}</section>;
}
