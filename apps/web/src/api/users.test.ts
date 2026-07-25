// Unit tests for the admin user-provisioning API wrapper. We stub `fetch`
// and a token in localStorage so the Bearer dance runs without a gateway.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// `auth/oauth` touches `location.origin` at module load; stub it before the
// static import chain pulls it in (default `node` test env has no location).
vi.mock("../auth/oauth", () => ({
  refreshTokens: vi.fn(),
}));

const store = new Map<string, string>();
vi.stubGlobal("localStorage", {
  getItem: (k: string) => store.get(k) ?? null,
  setItem: (k: string, v: string) => {
    store.set(k, v);
  },
  removeItem: (k: string) => {
    store.delete(k);
  },
  clear: () => {
    store.clear();
  },
});

import { createUser, deleteUser, listUsers, resetUserPassword } from "./users";

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

beforeEach(() => {
  // Mirrors auth/tokens.ts's per-key layout (readTokens reads these three).
  localStorage.setItem("gw_access_token", "access-1");
  localStorage.setItem("gw_refresh_token", "refresh-1");
  localStorage.setItem("gw_access_expires_at", String(Date.now() + 3_600_000));
});

afterEach(() => {
  vi.restoreAllMocks();
  store.clear();
});

describe("users admin API", () => {
  it("listUsers unwraps the {users} envelope", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse({
        users: [
          { id: 1, username: "owner", display_name: "Owner", role: "admin", created_at: 0 },
          { id: 2, username: "alice", display_name: null, role: "user", created_at: 1 },
        ],
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    const users = await listUsers();
    expect(users).toHaveLength(2);
    expect(users[1]).toMatchObject({ username: "alice", role: "user" });
    expect(fetchMock).toHaveBeenCalledWith(
      "/v1/admin/users",
      expect.objectContaining({ headers: expect.objectContaining({ Authorization: "Bearer access-1" }) }),
    );
  });

  it("createUser posts JSON and returns the new id", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse({ id: 7 }, 201));
    vi.stubGlobal("fetch", fetchMock);

    const id = await createUser({ username: "bob", password: "a-long-password", role: "user" });
    expect(id).toBe(7);
    const [, init] = fetchMock.mock.calls[0]!;
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body)).toMatchObject({ username: "bob", role: "user" });
  });

  it("createUser surfaces the server's message on 409", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        jsonResponse({ error: "username_taken", message: "username already taken" }, 409),
      ),
    );
    await expect(
      createUser({ username: "dup", password: "a-long-password", role: "user" }),
    ).rejects.toThrow("username already taken");
  });

  it("deleteUser hits the id path with DELETE", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetchMock);
    await deleteUser(5);
    expect(fetchMock).toHaveBeenCalledWith(
      "/v1/admin/users/5",
      expect.objectContaining({ method: "DELETE" }),
    );
  });

  it("resetUserPassword posts the new password to the password subpath", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetchMock);
    await resetUserPassword(5, "brand-new-password");
    const [path, init] = fetchMock.mock.calls[0]!;
    expect(path).toBe("/v1/admin/users/5/password");
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body)).toEqual({ password: "brand-new-password" });
  });
});
