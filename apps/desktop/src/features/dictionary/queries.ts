import { useInfiniteQuery } from "@tanstack/react-query";
import { listDictionary } from "@/api/dictionary";
import { useAccountEpoch } from "@/features/accounts/account-context";

export function useDictionary(query: string) {
  const { epoch } = useAccountEpoch();
  const normalizedQuery = query.trim();
  return useInfiniteQuery({
    queryKey: ["dictionary", epoch, normalizedQuery] as const,
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => listDictionary({
      query: normalizedQuery || undefined,
      cursor: pageParam,
      limit: 50,
    }),
    getNextPageParam: (page) => page.next_cursor ?? undefined,
  });
}
