import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { Alert, AlertAction, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { useI18n } from "@/i18n/I18nProvider";
import { cn } from "@/lib/utils";

export type LearningNotification = {
  generation: number;
  added_terms: string[];
};

export function DictionaryLearningNotice({ capsule = false }: { capsule?: boolean }) {
  const { t } = useI18n();
  const [notification, setNotification] = useState<LearningNotification | null>(null);
  const [undoingGeneration, setUndoingGeneration] = useState<number | null>(null);
  const [undoErrorGeneration, setUndoErrorGeneration] = useState<number | null>(null);

  const keepNewest = (
    current: LearningNotification | null,
    incoming: LearningNotification | null,
  ) => !incoming || (current && current.generation > incoming.generation) ? current : incoming;

  useEffect(() => {
    let active = true;
    let dismissedThrough = 0;
    const accept = (incoming: LearningNotification | null) => incoming && incoming.generation > dismissedThrough ? incoming : null;
    const disposers: Array<() => void> = [];
    void invoke<LearningNotification | null>("learning_notification_status")
      .then((current) => { if (active) setNotification((shown) => keepNewest(shown, accept(current))); })
      .catch(() => undefined);
    void listen<LearningNotification>("dictionary-learning-completed", ({ payload }) => {
      if (active) setNotification((current) => keepNewest(current, accept(payload)));
    }).then((dispose) => { if (active) disposers.push(dispose); else dispose(); });
    for (const event of ["dictionary-learning-dismissed", "dictionary-learning-undone"]) {
      void listen<number>(event, ({ payload }) => {
        dismissedThrough = Math.max(dismissedThrough, payload);
        if (active) setNotification((current) => current && current.generation <= payload ? null : current);
      }).then((dispose) => { if (active) disposers.push(dispose); else dispose(); });
    }
    return () => {
      active = false;
      disposers.forEach((dispose) => dispose());
    };
  }, []);

  if (!notification) return null;
  const preview = notification.added_terms.slice(0, 3).join(" · ");

  const undo = async () => {
    const generation = notification.generation;
    setUndoingGeneration(generation);
    setUndoErrorGeneration(null);
    try {
      await invoke<number>("undo_latest_dictionary_learning", {
        generation,
      });
      setNotification((current) => current?.generation === generation ? null : current);
    } catch {
      setUndoErrorGeneration(generation);
    } finally {
      setUndoingGeneration((current) => current === generation ? null : current);
    }
  };

  return (
    <aside className={cn(capsule ? "w-full" : "fixed top-4 right-4 w-80")} aria-live="polite">
      <Alert className="shadow-lg">
        <AlertTitle>{t("dictionary.learning.notice_title", { count: notification.added_terms.length })}</AlertTitle>
        <AlertDescription className="truncate">
          {undoErrorGeneration === notification.generation
            ? t("dictionary.learning.undo_failed")
            : preview}
        </AlertDescription>
        <AlertAction>
          <Button
            variant="ghost"
            size="sm"
            disabled={undoingGeneration === notification.generation}
            onClick={() => void undo()}
          >
            {undoingGeneration === notification.generation
              ? t("dictionary.learning.undoing")
              : t("dictionary.learning.undo")}
          </Button>
        </AlertAction>
      </Alert>
    </aside>
  );
}
