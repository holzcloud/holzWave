// The pure part of `Confirm it is you` (docs/design.md 6.29): when a refused change asks for a recent sign-in, which ways the
// account has to give one, and the sentences of the layer. No DOM and no network here, so node tests it on the compiled file.

/** The words of the layer. */
export const STEP_UP_TITLE = "Confirm it is you";
export const NO_METHOD = "There is no way to confirm it here. Sign out and sign in again.";

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);

/**
 * Whether a failed request is the server asking for a recent sign-in: the status 403 and the member `step_up` set to `true`
 * in the answer. A 403 without it is a refusal that a sign-in does not change.
 */
export function needsStepUp(failure: unknown): boolean {
  if (!isObject(failure)) return false;
  return failure.status === 403 && isObject(failure.body) && failure.body.step_up === true;
}

/** A provider id goes into a path: lower-case letters, digits, dash and underscore, 1 to 32 characters. */
const PROVIDER_ID = /^[a-z0-9_-]{1,32}$/;

/** One way to confirm. */
export type Method = { kind: "passkey" } | { kind: "password" } | { kind: "provider"; id: string; name: string };

/** What the layer needs to know about the account. */
export interface StepUpMe {
  /** The account name shown in the sentence. */
  name: string;
  password: boolean;
  /** The number of passkeys. */
  passkeys: number;
  oidc: { id: string; name: string } | null;
}

/** The part of the answer of `GET /api/me` the layer reads, or null when it is not that shape. */
export function readStepUpMe(raw: unknown): StepUpMe | null {
  if (!isObject(raw) || !isObject(raw.account)) return null;
  const name = raw.account.username;
  if (typeof name !== "string" || name === "" || name.length > 128) return null;
  const passkeys = Array.isArray(raw.passkeys) ? Math.min(raw.passkeys.length, 64) : 0;
  let oidc: StepUpMe["oidc"] = null;
  if (isObject(raw.oidc) && typeof raw.oidc.id === "string" && typeof raw.oidc.name === "string") {
    if (raw.oidc.name !== "" && raw.oidc.name.length <= 120) oidc = { id: raw.oidc.id, name: raw.oidc.name };
  }
  return { name, password: raw.password === true, passkeys, oidc };
}

/** The ways the account has, passkey first, then password, then the provider. A provider whose id is not safe in a path is dropped. */
export function stepUpMethods(me: { password: boolean; passkeys: number | readonly unknown[]; oidc: { id: string; name: string } | null }): Method[] {
  const out: Method[] = [];
  const keys = typeof me.passkeys === "number" ? me.passkeys : me.passkeys.length;
  if (keys > 0) out.push({ kind: "passkey" });
  if (me.password) out.push({ kind: "password" });
  if (me.oidc !== null && PROVIDER_ID.test(me.oidc.id)) out.push({ kind: "provider", id: me.oidc.id, name: me.oidc.name });
  return out;
}

/** The path that signs a person in again through their provider: only built for an id that passed `stepUpMethods`. */
export function providerPath(id: string): string | null {
  return PROVIDER_ID.test(id) ? `/auth/oidc/${id}/start` : null;
}

/** The sentence under the title: it names the account in the first words, so that no other dialog can pass for this layer. */
export function leadParts(methods: readonly Method[]): { before: string; after: string } {
  const has = (k: Method["kind"]) => methods.some((m) => m.kind === k);
  if (has("password")) return { before: "Signed in as ", after: ". Enter your password to make this change." };
  if (has("passkey")) return { before: "Signed in as ", after: ". Use your passkey to make this change." };
  return { before: "Signed in as ", after: ". Sign in again to make this change." };
}
