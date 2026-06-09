// Identity hook: fetches /v1/whoami once authenticated and caches it.
//
// Drives role-gated UI (PR A of the user-system plan). The query is
// disabled until there's a token, so it never fires on the login screen.
// While loading or on error we deliberately assume the LEAST privilege
// (not admin) so admin-only surfaces never flash in before identity is
// confirmed.

import { useQuery } from "@tanstack/react-query";

import { Role, Whoami, whoami } from "../api/client";
import { useAuth } from "./AuthContext";

export function useWhoami() {
  const { tokens } = useAuth();
  return useQuery<Whoami>({
    queryKey: ["whoami"],
    queryFn: whoami,
    enabled: !!tokens,
    staleTime: 5 * 60_000,
  });
}

/** The caller's role, or `undefined` until whoami resolves. */
export function useRole(): Role | undefined {
  return useWhoami().data?.role;
}

/** True only once whoami confirms an admin — fails closed while loading. */
export function useIsAdmin(): boolean {
  return useRole() === "admin";
}
