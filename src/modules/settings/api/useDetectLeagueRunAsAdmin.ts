import { useQuery } from "@tanstack/react-query";

import { api, type AppError } from "@/lib/tauri";
import { queryFn } from "@/utils/query";

import { settingsKeys } from "./keys";

/**
 * Hook to detect whether League is configured to launch as administrator (an
 * AppCompatFlags `RUNASADMIN` layer on its executable).
 *
 * When true, the patcher auto-elevates the injection host regardless of the
 * `elevateInjector` setting, so the settings UI uses this to explain why a UAC
 * prompt may appear. The compat flag rarely changes, so we keep it fresh for a
 * few minutes.
 */
export function useDetectLeagueRunAsAdmin() {
  return useQuery<boolean, AppError>({
    queryKey: settingsKeys.leagueRunAsAdmin(),
    queryFn: queryFn(api.detectLeagueRunAsAdmin),
    staleTime: 5 * 60 * 1000,
    retry: false,
  });
}
