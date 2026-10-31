// WebAuthn on the login page: the conversions between the JSON the server speaks (binary members as base64url) and
// the binary form `navigator.credentials` wants, and back. Pure functions; no DOM and no network, so node tests them.
//
// The conversion is done here by hand and always, in every browser: `PublicKeyCredential.parseRequestOptionsFromJSON`
// exists in some engines only, and one path that the unit tests cover beats two paths that differ by engine. Base64url
// is the standard's own alphabet without padding, written over `atob` and `btoa`; there is no library.
//
// The PRF result (the authenticator's secret output for the salt the server sent) is added to the body only when the
// browser returned one for this ceremony; the page never invents it and sends it nowhere else.

const B64URL = /^[A-Za-z0-9_-]*$/;

/** The bytes of an `ArrayBuffer` or of a typed array / `DataView`, as a `Uint8Array` over the same memory. */
function bytesOf(value: unknown): Uint8Array | null {
  if (ArrayBuffer.isView(value)) return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  if (Object.prototype.toString.call(value) === "[object ArrayBuffer]") return new Uint8Array(value as ArrayBuffer);
  return null;
}

/** Base64url without padding. */
export function b64urlEncode(value: ArrayBuffer | ArrayBufferView): string {
  const bytes = bytesOf(value);
  if (bytes === null) throw new TypeError("not a buffer");
  let binary = "";
  for (const b of bytes) binary += String.fromCharCode(b);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** Bytes of a base64url string. Padding, `+`, `/` and any other character are refused with a `TypeError`. */
export function b64urlDecode(text: string): Uint8Array {
  if (typeof text !== "string" || !B64URL.test(text) || text.length % 4 === 1) throw new TypeError("not base64url");
  const padded = text.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - (text.length % 4)) % 4);
  const binary = atob(padded);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
  return out;
}

type Json = Record<string, unknown>;

const isObject = (v: unknown): v is Json => typeof v === "object" && v !== null && !Array.isArray(v);

function obj(value: unknown, what: string): Json {
  if (!isObject(value)) throw new TypeError(`${what} is not an object`);
  return value;
}

function list(value: unknown, what: string): unknown[] {
  if (!Array.isArray(value)) throw new TypeError(`${what} is not a list`);
  return value;
}

/**
 * The options of a ceremony as the server answers them: the library's own text is `{"publicKey": {...}}`, and the page takes
 * the members of `publicKey` (a bare object, as the scripted servers of the page tests answer, is taken as it is).
 */
function publicKeyOf(raw: unknown): Json {
  const o = obj(raw, "options");
  return isObject(o.publicKey) && o.challenge === undefined ? o.publicKey : o;
}

/** A base64url member as an `ArrayBuffer`. */
function buf(value: unknown, what: string): ArrayBuffer {
  if (typeof value !== "string") throw new TypeError(`${what} is not a string`);
  return b64urlDecode(value).buffer as ArrayBuffer;
}

/** `{first, second?}` of the PRF extension, both members binary. */
function prfValues(value: unknown, what: string): Json {
  const v = obj(value, what);
  const out: Json = { ...v, first: buf(v.first, `${what}.first`) };
  if (v.second !== undefined) out.second = buf(v.second, `${what}.second`);
  return out;
}

/** `extensions` with the binary members of `prf` converted; every other member is kept as it is. */
function extensions(value: unknown): Json {
  const ext = obj(value, "extensions");
  const out: Json = { ...ext };
  if (ext.prf !== undefined) {
    const prf = obj(ext.prf, "extensions.prf");
    const next: Json = { ...prf };
    if (prf.eval !== undefined) next.eval = prfValues(prf.eval, "extensions.prf.eval");
    if (prf.evalByCredential !== undefined) {
      const byCredential = obj(prf.evalByCredential, "extensions.prf.evalByCredential");
      const converted: Json = {};
      for (const key of Object.keys(byCredential)) converted[key] = prfValues(byCredential[key], `evalByCredential.${key}`);
      next.evalByCredential = converted;
    }
    out.prf = next;
  }
  return out;
}

/** A list of `{id, type, transports?}` with `id` converted. */
function descriptors(value: unknown, what: string): Json[] {
  return list(value, what).map((item) => {
    const d = obj(item, `${what} entry`);
    return { ...d, id: buf(d.id, `${what}.id`) };
  });
}

/** The options of `POST /auth/passkey/begin` in the form `navigator.credentials.get({ publicKey })` takes. Throws a `TypeError` for a member of the wrong type. */
export function decodeRequestOptions(raw: unknown): Json {
  const o = publicKeyOf(raw);
  const out: Json = { ...o, challenge: buf(o.challenge, "challenge") };
  if (o.allowCredentials !== undefined) out.allowCredentials = descriptors(o.allowCredentials, "allowCredentials");
  if (o.extensions !== undefined) out.extensions = extensions(o.extensions);
  return out;
}

/** The creation options of a set-up in the form `navigator.credentials.create({ publicKey })` takes. Throws a `TypeError` for a member of the wrong type. */
export function decodeCreationOptions(raw: unknown): Json {
  const o = publicKeyOf(raw);
  const out: Json = { ...o, challenge: buf(o.challenge, "challenge") };
  const user = obj(o.user, "user");
  out.user = { ...user, id: buf(user.id, "user.id") };
  if (o.excludeCredentials !== undefined) out.excludeCredentials = descriptors(o.excludeCredentials, "excludeCredentials");
  if (o.extensions !== undefined) out.extensions = extensions(o.extensions);
  return out;
}

/** The PRF output the browser returned for this ceremony, base64url, or undefined when there is none. */
function prfOf(credential: Json): string | undefined {
  const get = credential.getClientExtensionResults;
  if (typeof get !== "function") return undefined;
  const results = (get as () => unknown).call(credential);
  if (!isObject(results) || !isObject(results.prf) || !isObject(results.prf.results)) return undefined;
  const first = bytesOf(results.prf.results.first);
  return first !== null && first.byteLength > 0 ? b64urlEncode(first) : undefined;
}

function head(credential: unknown): { c: Json; response: Json; id: string; type: string } {
  const c = obj(credential, "credential");
  const response = obj(c.response, "credential.response");
  if (typeof c.id !== "string" || c.id === "") throw new TypeError("credential.id is not a string");
  if (typeof c.type !== "string") throw new TypeError("credential.type is not a string");
  return { c, response, id: c.id, type: c.type };
}

const bin = (value: unknown, what: string): string => {
  const bytes = bytesOf(value);
  if (bytes === null) throw new TypeError(`${what} is not a buffer`);
  return b64urlEncode(bytes);
};

/** The assertion of a sign-in as the body of `POST /auth/passkey/finish`: `{ credential, prf? }`. */
export function encodeAssertion(credential: unknown): { credential: Json; prf?: string } {
  const { c, response, id, type } = head(credential);
  const handle = response.userHandle;
  const body: { credential: Json; prf?: string } = {
    credential: {
      id,
      rawId: bin(c.rawId, "rawId"),
      type,
      response: {
        authenticatorData: bin(response.authenticatorData, "authenticatorData"),
        clientDataJSON: bin(response.clientDataJSON, "clientDataJSON"),
        signature: bin(response.signature, "signature"),
        userHandle: handle === null || handle === undefined ? null : bin(handle, "userHandle"),
      },
    },
  };
  const prf = prfOf(c);
  if (prf !== undefined) body.prf = prf;
  return body;
}

/** The attestation of a registration as the body of a `finish` step: `{ credential, prf? }`. */
export function encodeAttestation(credential: unknown): { credential: Json; prf?: string } {
  const { c, response, id, type } = head(credential);
  const out: Json = {
    attestationObject: bin(response.attestationObject, "attestationObject"),
    clientDataJSON: bin(response.clientDataJSON, "clientDataJSON"),
  };
  const getTransports = response.getTransports;
  if (typeof getTransports === "function") {
    const transports = (getTransports as () => unknown).call(response);
    if (Array.isArray(transports) && transports.every((t) => typeof t === "string")) out.transports = transports;
  }
  const body: { credential: Json; prf?: string } = { credential: { id, rawId: bin(c.rawId, "rawId"), type, response: out } };
  const prf = prfOf(c);
  if (prf !== undefined) body.prf = prf;
  return body;
}
