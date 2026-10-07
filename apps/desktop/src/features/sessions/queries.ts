import { useInfiniteQuery } from "@tanstack/react-query";
import { getSession, listSessions, searchSessions, type SessionSource } from "@/api/sessions";
import { useQuery } from "@tanstack/react-query";
import { useAccountEpoch } from "@/features/accounts/account-context";
import { getSessionCleanupDetail, getSessionWorkspaceDetail } from "@/api/transport";

export function useSessions(source?: SessionSource) {
  const { epoch } = useAccountEpoch();
  return useInfiniteQuery({
    queryKey: ["sessions", epoch, source] as const,
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => listSessions({ cursor: pageParam, source, limit: 20 }),
    getNextPageParam: (page) => page.next_cursor ?? undefined,
  });
}

export function useSessionSearch(term: string, source?: SessionSource) {
  const { epoch } = useAccountEpoch();
  return useInfiniteQuery({
    queryKey: ["sessions-search", epoch, term, source] as const,
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => searchSessions({ q: term.trim(), cursor: pageParam, source, limit: 20 }),
    getNextPageParam: (page) => page.next_cursor ?? undefined,
    enabled: term.trim().length > 0,
  });
}

export function useSession(id?: string) {
  const { epoch } = useAccountEpoch();
  return useQuery({
    queryKey: ["session", epoch, id] as const,
    queryFn: () => getSession(id!),
    enabled: Boolean(id),
  });
}

export function useSessionWorkspaceDetail(id?: string, enabled = true) {
  const { epoch } = useAccountEpoch();
  return useQuery({
    queryKey: ["session-workspace-detail", epoch, id] as const,
    queryFn: () => getSessionWorkspaceDetail(id!),
    enabled: Boolean(id) && enabled,
  });
}

export function useSessionCleanupDetail(id?: string, enabled = true) {
  const { epoch } = useAccountEpoch();
  return useQuery({
    queryKey: ["session-cleanup-detail", epoch, id] as const,
    queryFn: () => getSessionCleanupDetail(id!),
    enabled: Boolean(id) && enabled,
    gcTime: 0,
  });
}

export function contextThumbnailQueryKey(epoch: number, id: string, sequence: number, resourceIndex: number) {
  return ["context-thumbnail", epoch, id, sequence, resourceIndex] as const;
}
