// Admin user provisioning (PR B of the user-system plan). Talks to the
// gateway's admin-tier `/v1/admin/users` surface, which is gated by
// `require_admin` — a non-admin caller 403s, so these helpers are only
// wired from the admin-only Users panel.
//
// Auth piggybacks on the same Bearer-with-refresh dance as library.ts /
// events.ts (client.ts's apiFetch is GET-only), copied locally for the
// same reason noted there.

import { refreshTokens } from "../auth/oauth";
import { clearTokens, readTokens } from "../auth/tokens";
import type { Role } from "./client";

class AuthError extends Error {}

async function apiFetch(path: string, init?: RequestInit): Promise<Response> {
  const tokens = readTokens();
  if (!tokens) throw new AuthError("not signed in");
  const doFetch = (token: string) =>
    fetch(path, {
      ...init,
      headers: { ...(init?.headers ?? {}), Authorization: `Bearer ${token}` },
    });

  let res = await doFetch(tokens.accessToken);
  if (res.status === 401) {
    if (!tokens.refreshToken) {
      clearTokens();
      throw new AuthError("guest session expired");
    }
    try {
      await refreshTokens(tokens.refreshToken);
    } catch {
      clearTokens();
      throw new AuthError("session expired");
    }
    const refreshed = readTokens();
    if (!refreshed) throw new AuthError("session expired");
    res = await doFetch(refreshed.accessToken);
    if (res.status === 401) {
      clearTokens();
      throw new AuthError("session expired");
    }
  }
  return res;
}

/** A real account as the admin list returns it (no credential material). */
export interface AdminUser {
  id: number;
  username: string | null;
  display_name: string | null;
  role: Role;
  created_at: number;
}

/** Roles the provisioning UI can assign. Guests come from the guest-code
 *  flow (PR D), never this endpoint. */
export type ProvisionRole = "admin" | "user";

export interface NewAccount {
  username: string;
  display_name?: string;
  password: string;
  role: ProvisionRole;
}

async function errorMessage(res: Response, fallback: string): Promise<string> {
  try {
    const body = (await res.json()) as { message?: string };
    if (body.message) return body.message;
  } catch {
    /* non-JSON body */
  }
  return `${fallback} (HTTP ${res.status})`;
}

export async function listUsers(): Promise<AdminUser[]> {
  const res = await apiFetch("/v1/admin/users");
  if (!res.ok) throw new Error(await errorMessage(res, "could not load users"));
  const body = (await res.json()) as { users: AdminUser[] };
  return body.users;
}

/** Create a real account. Returns the new id. Throws a friendly message on
 *  a duplicate username (409) or validation failure (400). */
export async function createUser(input: NewAccount): Promise<number> {
  const res = await apiFetch("/v1/admin/users", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(input),
  });
  if (!res.ok) throw new Error(await errorMessage(res, "could not create user"));
  const body = (await res.json()) as { id: number };
  return body.id;
}

export async function deleteUser(id: number): Promise<void> {
  const res = await apiFetch(`/v1/admin/users/${id}`, { method: "DELETE" });
  if (!res.ok) throw new Error(await errorMessage(res, "could not delete user"));
}

export async function resetUserPassword(id: number, password: string): Promise<void> {
  const res = await apiFetch(`/v1/admin/users/${id}/password`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ password }),
  });
  if (!res.ok) throw new Error(await errorMessage(res, "could not reset password"));
}

export { AuthError };
