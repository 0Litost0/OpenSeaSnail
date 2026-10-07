import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import App from "./App";
import { RealtimeProgressCapsuleHarness } from "./features/recording/RealtimeProgressCapsuleHarness";
import { RealtimeProgressCapsuleSmokeHarness } from "./features/recording/RealtimeProgressCapsuleSmokeHarness";
import "./index.css";
import { I18nProvider } from "./i18n/I18nProvider";
import { DictionaryLearningNotice } from "./features/dictionary/DictionaryLearningNotice";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: 1,
      refetchOnWindowFocus: false,
    },
  },
});

const isCapsuleWindow = new URLSearchParams(window.location.search).get("capsule") === "1";
const isLearningCapsuleWindow = new URLSearchParams(window.location.search).get("learningCapsule") === "1";
const isCapsuleSmoke = import.meta.env.VITE_CAPSULE_SMOKE === "true";
if (isCapsuleWindow) document.documentElement.dataset.windowSurface = "transparent";
const content = isLearningCapsuleWindow
  ? <main className="flex h-screen items-center bg-background p-1 text-foreground"><DictionaryLearningNotice capsule /></main>
  : isCapsuleWindow
  ? isCapsuleSmoke ? <RealtimeProgressCapsuleSmokeHarness /> : <RealtimeProgressCapsuleHarness />
  : <App />;

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      {isCapsuleSmoke && isCapsuleWindow ? content : <I18nProvider>{content}</I18nProvider>}
    </QueryClientProvider>
  </StrictMode>,
);
