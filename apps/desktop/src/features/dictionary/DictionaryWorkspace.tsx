import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ClipboardPasteIcon, FileDownIcon, FileUpIcon, PencilIcon, PlusIcon, SearchIcon, Trash2Icon } from "lucide-react";
import { useRef, useState, type FormEvent } from "react";
import {
  addDictionaryTerms,
  clearDictionary,
  deleteDictionaryEntry,
  editDictionaryEntry,
  importDictionary,
  previewDictionaryImport,
  type DictionaryImportPreview,
} from "@/api/dictionary";
import { ApiError, localizedError } from "@/api/errors";
import { exportDictionaryToFile } from "@/api/transport";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "@/components/ui/empty";
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Textarea } from "@/components/ui/textarea";
import { useI18n } from "@/i18n/I18nProvider";
import { useDictionary } from "./queries";

function PreviewStats({ preview }: { preview: DictionaryImportPreview }) {
  const { t } = useI18n();
  const stats = [
    ["dictionary.import.parsed", preview.parsed_count],
    ["dictionary.import.valid", preview.valid_count],
    ["dictionary.import.added", preview.added_count],
    ["dictionary.import.promoted", preview.promoted_count],
    ["dictionary.import.skipped", preview.skipped_count],
  ] as const;
  return <div className="grid grid-cols-2 gap-2 sm:grid-cols-5">
    {stats.map(([label, value]) => <div key={label} className="rounded-lg bg-muted p-3"><p className="text-xs text-muted-foreground">{t(label)}</p><p className="text-lg font-semibold tabular-nums">{value}</p></div>)}
  </div>;
}

export function DictionaryWorkspace() {
  const { t } = useI18n();
  const queryClient = useQueryClient();
  const [search, setSearch] = useState("");
  const [term, setTerm] = useState("");
  const [editingId, setEditingId] = useState<string>();
  const [editingTerm, setEditingTerm] = useState("");
  const [csv, setCsv] = useState<string>();
  const [preview, setPreview] = useState<DictionaryImportPreview>();
  const [pasteOpen, setPasteOpen] = useState(false);
  const [pastedCsv, setPastedCsv] = useState("");
  const [pastePreviewing, setPastePreviewing] = useState(false);
  const [message, setMessage] = useState<string>();
  const [error, setError] = useState<string>();
  const importSelection = useRef(0);
  const dictionary = useDictionary(search);
  const entries = dictionary.data?.pages.flatMap((page) => page.items) ?? [];

  const refresh = async () => {
    await queryClient.invalidateQueries({ queryKey: ["dictionary"] });
  };
  const mutation = useMutation({
    mutationFn: addDictionaryTerms,
    onSuccess: async (result) => {
      setTerm("");
      setError(undefined);
      setMessage(t("dictionary.add.result", { added: result.added.length, promoted: result.promoted.length, skipped: result.skipped_count }));
      await refresh();
    },
    onError: (reason) => { setMessage(undefined); setError(localizedError(t, reason)); },
  });
  const editMutation = useMutation({
    mutationFn: ({ id, value }: { id: string; value: string }) => editDictionaryEntry(id, value),
    onSuccess: async () => { setEditingId(undefined); setError(undefined); await refresh(); },
    onError: (reason) => setError(localizedError(t, reason)),
  });
  const deleteMutation = useMutation({
    mutationFn: deleteDictionaryEntry,
    onSuccess: refresh,
    onError: (reason) => setError(localizedError(t, reason)),
  });
  const clearMutation = useMutation({
    mutationFn: clearDictionary,
    onSuccess: async () => { setMessage(t("dictionary.clear.done")); setError(undefined); await refresh(); },
    onError: (reason) => setError(localizedError(t, reason)),
  });
  const importMutation = useMutation({
    mutationFn: importDictionary,
    onSuccess: async (result) => {
      setCsv(undefined);
      setPreview(undefined);
      setError(undefined);
      setMessage(t("dictionary.import.result", { added: result.added.length, promoted: result.promoted.length, skipped: result.skipped_count }));
      await refresh();
    },
    onError: (reason) => setError(localizedError(t, reason)),
  });

  const submitTerm = (event: FormEvent) => {
    event.preventDefault();
    if (term.trim()) mutation.mutate([term]);
  };
  const selectCsv = async (file?: File) => {
    if (!file) return;
    const selection = ++importSelection.current;
    setError(undefined);
    setMessage(undefined);
    setPreview(undefined);
    // 文件选择取代粘贴输入，避免两个导入来源的预览状态互相覆盖。
    setPasteOpen(false);
    setPastedCsv("");
    setPastePreviewing(false);
    try {
      const bytes = await file.arrayBuffer();
      let content: string;
      try {
        content = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
      } catch {
        throw new ApiError("dictionary_csv_invalid");
      }
      const result = await previewDictionaryImport(content);
      if (selection !== importSelection.current) return;
      setCsv(content);
      setPreview(result);
    } catch (reason) {
      if (selection !== importSelection.current) return;
      setCsv(undefined);
      setError(localizedError(t, reason));
    }
  };
  const exportCsv = async () => {
    setError(undefined);
    try {
      const saved = await exportDictionaryToFile();
      setMessage(t(saved ? "dictionary.export.saved" : "dictionary.export.cancelled"));
    } catch (reason) {
      setMessage(undefined);
      setError(localizedError(t, reason));
    }
  };
  const previewPastedCsv = async () => {
    const selection = ++importSelection.current;
    setError(undefined);
    setMessage(undefined);
    setPreview(undefined);
    setPastePreviewing(true);
    try {
      const result = await previewDictionaryImport(pastedCsv);
      if (selection !== importSelection.current) return;
      setCsv(pastedCsv);
      setPreview(result);
      setPasteOpen(false);
      setPastedCsv("");
    } catch (reason) {
      if (selection !== importSelection.current) return;
      setCsv(undefined);
      setError(localizedError(t, reason));
    } finally {
      if (selection === importSelection.current) setPastePreviewing(false);
    }
  };

  return <section className="flex min-h-0 flex-col gap-4" aria-labelledby="dictionary-title">
    <div className="flex flex-wrap items-start justify-between gap-3">
      <div><h2 id="dictionary-title" className="font-heading text-xl font-semibold">{t("dictionary.title")}</h2><p className="text-sm text-muted-foreground">{t("dictionary.description")}</p></div>
      <div className="flex flex-wrap gap-2">
        <Button variant="outline" asChild><label className="cursor-pointer"><FileUpIcon />{t("dictionary.import.choose")}<input className="sr-only" type="file" accept=".csv,text/csv" onChange={(event) => { void selectCsv(event.target.files?.[0]); event.currentTarget.value = ""; }} /></label></Button>
        <Button variant="outline" onClick={() => { setPasteOpen((open) => !open); setError(undefined); }}><ClipboardPasteIcon />{t("dictionary.import.paste")}</Button>
        <Button variant="outline" onClick={() => void exportCsv()}><FileDownIcon />{t("dictionary.export")}</Button>
        <AlertDialog>
          <AlertDialogTrigger asChild><Button variant="destructive" disabled={clearMutation.isPending}><Trash2Icon />{t("dictionary.clear")}</Button></AlertDialogTrigger>
          <AlertDialogContent>
            <AlertDialogHeader><AlertDialogTitle>{t("dictionary.clear.title")}</AlertDialogTitle><AlertDialogDescription>{t("dictionary.clear.description")}</AlertDialogDescription></AlertDialogHeader>
            <AlertDialogFooter><AlertDialogCancel>{t("dictionary.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => clearMutation.mutate()}>{t("dictionary.clear.confirm")}</AlertDialogAction></AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
      </div>
    </div>

    <Card>
      <CardHeader><CardTitle>{t("dictionary.add.title")}</CardTitle><CardDescription>{t("dictionary.add.description")}</CardDescription></CardHeader>
      <CardContent><form onSubmit={submitTerm}><FieldGroup><Field orientation="horizontal"><FieldLabel htmlFor="dictionary-term" className="sr-only">{t("dictionary.term")}</FieldLabel><Input id="dictionary-term" value={term} onChange={(event) => setTerm(event.target.value)} placeholder={t("dictionary.add.placeholder")} maxLength={128} /><Button type="submit" disabled={mutation.isPending || !term.trim()}><PlusIcon />{t("dictionary.add")}</Button></Field><FieldDescription>{t("dictionary.add.hint")}</FieldDescription></FieldGroup></form></CardContent>
    </Card>

    {pasteOpen && <Card>
      <CardHeader><CardTitle>{t("dictionary.import.paste_title")}</CardTitle><CardDescription>{t("dictionary.import.paste_description")}</CardDescription></CardHeader>
      <CardContent><FieldGroup><Field><FieldLabel htmlFor="dictionary-paste" className="sr-only">{t("dictionary.import.paste_label")}</FieldLabel><Textarea id="dictionary-paste" className="font-mono" rows={6} value={pastedCsv} onChange={(event) => setPastedCsv(event.target.value)} placeholder={t("dictionary.import.paste_placeholder")} /></Field><div className="flex justify-end gap-2"><Button variant="outline" onClick={() => { setPasteOpen(false); setPastedCsv(""); }}>{t("dictionary.cancel")}</Button><Button onClick={() => void previewPastedCsv()} disabled={pastePreviewing || !pastedCsv.trim()}>{t("dictionary.import.preview_action")}</Button></div></FieldGroup></CardContent>
    </Card>}

    {preview && csv && <Card>
      <CardHeader><CardTitle>{t("dictionary.import.preview")}</CardTitle><CardDescription>{t("dictionary.import.preview_description")}</CardDescription></CardHeader>
      <CardContent className="flex flex-col gap-3"><PreviewStats preview={preview} /><p className="text-xs text-muted-foreground">{t("dictionary.import.final_note")}</p><div className="flex justify-end gap-2"><Button variant="outline" onClick={() => { setCsv(undefined); setPreview(undefined); }}>{t("dictionary.cancel")}</Button><Button onClick={() => importMutation.mutate(csv)} disabled={importMutation.isPending}>{t("dictionary.import.confirm")}</Button></div></CardContent>
    </Card>}

    {(error || message) && <p role={error ? "alert" : "status"} className={error ? "text-sm text-destructive" : "text-sm text-muted-foreground"}>{error ?? message}</p>}

    <Field>
      <FieldLabel htmlFor="dictionary-search" className="sr-only">{t("dictionary.search")}</FieldLabel>
      <div className="relative"><SearchIcon aria-hidden="true" className="absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" /><Input id="dictionary-search" className="pl-9" value={search} onChange={(event) => setSearch(event.target.value)} placeholder={t("dictionary.search.placeholder")} maxLength={128} /></div>
    </Field>

    {dictionary.isPending ? <p className="text-sm text-muted-foreground">{t("common.loading")}</p> : dictionary.error ? <div className="flex items-center gap-2"><p role="alert" className="text-sm text-destructive">{localizedError(t, dictionary.error)}</p><Button variant="outline" size="sm" onClick={() => void dictionary.refetch()}>{t("common.retry")}</Button></div> : entries.length === 0 ? <Empty className="border"><EmptyHeader><EmptyMedia variant="icon"><SearchIcon /></EmptyMedia><EmptyTitle>{t(search.trim() ? "dictionary.empty.search" : "dictionary.empty")}</EmptyTitle><EmptyDescription>{t(search.trim() ? "dictionary.empty.search_description" : "dictionary.empty.description")}</EmptyDescription></EmptyHeader></Empty> : <div className="rounded-xl border">
      <Table>
        <TableHeader><TableRow><TableHead>{t("dictionary.term")}</TableHead><TableHead>{t("dictionary.source")}</TableHead><TableHead className="w-36 text-right">{t("dictionary.actions")}</TableHead></TableRow></TableHeader>
        <TableBody>{entries.map((entry) => <TableRow key={entry.id}>
          <TableCell className="whitespace-normal">{editingId === entry.id ? <Input aria-label={t("dictionary.edit.value")} value={editingTerm} onChange={(event) => setEditingTerm(event.target.value)} maxLength={128} /> : entry.term}</TableCell>
          <TableCell><Badge variant={entry.source === "learned" ? "secondary" : "outline"}>{t(entry.source === "learned" ? "dictionary.source.learned" : "dictionary.source.manual")}</Badge></TableCell>
          <TableCell><div className="flex justify-end gap-1">{editingId === entry.id ? <><Button size="sm" onClick={() => editMutation.mutate({ id: entry.id, value: editingTerm })} disabled={!editingTerm.trim() || editMutation.isPending}>{t("dictionary.save")}</Button><Button size="sm" variant="ghost" onClick={() => setEditingId(undefined)}>{t("dictionary.cancel")}</Button></> : <><Button size="icon-sm" variant="ghost" aria-label={t("dictionary.edit")} onClick={() => { setEditingId(entry.id); setEditingTerm(entry.term); setError(undefined); }}><PencilIcon /></Button><Button size="icon-sm" variant="ghost" aria-label={t("dictionary.delete")} onClick={() => deleteMutation.mutate(entry.id)}><Trash2Icon /></Button></>}</div></TableCell>
        </TableRow>)}</TableBody>
      </Table>
      {dictionary.hasNextPage && <div className="flex justify-center border-t p-3"><Button variant="outline" onClick={() => void dictionary.fetchNextPage()} disabled={dictionary.isFetchingNextPage}>{t("dictionary.load_more")}</Button></div>}
    </div>}
  </section>;
}
