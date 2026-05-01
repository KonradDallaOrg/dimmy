// Cryptographic primitives — Ed25519 sign + base64url + SHA-256.
// Uses the Web Crypto API which is fully supported in Cloudflare Workers
// (no Node.js shims, no extra deps).
//
// IMPORTANT: this module is the production-side counterpart to
// `core/src/license.rs`'s verify path. Tokens minted here MUST be
// verifiable by the Rust client without any change to the format
// (header.payload.signature, base64url, no padding).

/// Stable header for all Dimmy License Tokens. Matches the literal
/// the Rust client checks.
const TOKEN_HEADER_JSON = '{"alg":"EdDSA","typ":"DLT"}';

/// Base64url encode (no padding) — matches Rust's
/// `base64::engine::general_purpose::URL_SAFE_NO_PAD`.
export function b64urlEncode(bytes: Uint8Array): string {
  let b64 = btoa(String.fromCharCode(...bytes));
  return b64.replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/// Base64url decode (no padding tolerated).
export function b64urlDecode(s: string): Uint8Array {
  const padded = s.replace(/-/g, "+").replace(/_/g, "/").padEnd(
    s.length + ((4 - (s.length % 4)) % 4),
    "="
  );
  const binary = atob(padded);
  return Uint8Array.from(binary, (c) => c.charCodeAt(0));
}

/// SHA-256 of UTF-8 string → hex (matches Rust `email_hash`).
export async function sha256Hex(input: string): Promise<string> {
  const data = new TextEncoder().encode(input);
  const hash = await crypto.subtle.digest("SHA-256", data);
  return [...new Uint8Array(hash)]
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

/// Hash an email with the same normalisation the client uses
/// (lowercase + trim). The Rust counterpart is in license.rs::email_hash.
export async function emailHash(email: string): Promise<string> {
  return sha256Hex(email.trim().toLowerCase());
}

/// Claims payload — keep field names short, must match the Rust
/// `Claims` struct in core/src/license.rs.
export interface Claims {
  v: number;
  lid: string;
  eh: string;
  tier: "trial" | "monthly" | "annual" | "lifetime";
  iat: number;
  exp: number;
  max_offline: number;
  did: string;
  scope: string[];
}

/// Import a 32-byte raw Ed25519 private key into the Web Crypto API.
/// Web Crypto requires PKCS#8 wrapping for the import; we wrap the raw
/// bytes with the standard ASN.1 PKCS#8 prefix for Ed25519.
async function importEd25519PrivateKey(rawBytes: Uint8Array): Promise<CryptoKey> {
  if (rawBytes.length !== 32) {
    throw new Error(`ed25519 private key must be 32 bytes, got ${rawBytes.length}`);
  }
  // PKCS#8 prefix for an Ed25519 private key followed by the 32-byte
  // raw seed (per RFC 8410 / 5958). Hard-coded because it never changes.
  const PKCS8_PREFIX = new Uint8Array([
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70,
    0x04, 0x22, 0x04, 0x20,
  ]);
  const pkcs8 = new Uint8Array(PKCS8_PREFIX.length + rawBytes.length);
  pkcs8.set(PKCS8_PREFIX, 0);
  pkcs8.set(rawBytes, PKCS8_PREFIX.length);
  return crypto.subtle.importKey("pkcs8", pkcs8, { name: "Ed25519" }, false, [
    "sign",
  ]);
}

/// Sign a Claims object into a JWT-like Ed25519 token. Output format:
///   base64url(header_json).base64url(claims_json).base64url(signature)
///
/// `privKeyB64` must be base64url-encoded 32 raw bytes (matches the
/// format the licensing-server PoC writes to `keys.bin` and prints to
/// stdout for embedding in `DIMMY_LICENSE_PUBKEY`).
export async function signToken(claims: Claims, privKeyB64: string): Promise<string> {
  const headerB64 = b64urlEncode(new TextEncoder().encode(TOKEN_HEADER_JSON));
  const payloadJson = JSON.stringify(claims);
  const payloadB64 = b64urlEncode(new TextEncoder().encode(payloadJson));
  const signingInput = new TextEncoder().encode(`${headerB64}.${payloadB64}`);

  const privKeyBytes = b64urlDecode(privKeyB64);
  const cryptoKey = await importEd25519PrivateKey(privKeyBytes);
  const sig = await crypto.subtle.sign("Ed25519", cryptoKey, signingInput);
  const sigB64 = b64urlEncode(new Uint8Array(sig));
  return `${headerB64}.${payloadB64}.${sigB64}`;
}

/// Verify a token with an explicit raw 32-byte public key
/// (base64url-encoded). This is what the Worker actually uses on
/// /api/refresh — it has the pubkey from env / D1 and validates
/// inbound tokens before re-issuing.
export async function verifyTokenWithPub(
  token: string,
  pubKeyB64: string
): Promise<Claims> {
  const parts = token.split(".");
  if (parts.length !== 3) {
    throw new Error(`expected 3 token segments, got ${parts.length}`);
  }
  const [headerB64, payloadB64, sigB64] = parts;
  const signingInput = new TextEncoder().encode(`${headerB64}.${payloadB64}`);

  const pubKeyBytes = b64urlDecode(pubKeyB64);
  if (pubKeyBytes.length !== 32) {
    throw new Error(`public key must be 32 bytes, got ${pubKeyBytes.length}`);
  }
  // Web Crypto needs SubjectPublicKeyInfo wrapping for raw Ed25519 import.
  const SPKI_PREFIX = new Uint8Array([
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
  ]);
  const spki = new Uint8Array(SPKI_PREFIX.length + pubKeyBytes.length);
  spki.set(SPKI_PREFIX, 0);
  spki.set(pubKeyBytes, SPKI_PREFIX.length);
  const cryptoKey = await crypto.subtle.importKey(
    "spki",
    spki,
    { name: "Ed25519" },
    false,
    ["verify"]
  );

  const sig = b64urlDecode(sigB64);
  const ok = await crypto.subtle.verify("Ed25519", cryptoKey, sig, signingInput);
  if (!ok) throw new Error("signature verify failed");

  const payload = b64urlDecode(payloadB64);
  const claims: Claims = JSON.parse(new TextDecoder().decode(payload));
  if (claims.v !== 1) throw new Error(`unsupported schema v=${claims.v}`);
  return claims;
}

/// 26-char Crockford base32 ULID. Mirrors core/src/license_server.rs's
/// `ulid_string` so license_ids and device_ids look the same regardless
/// of which implementation issued them.
export function ulid(): string {
  const ALPHA = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
  const bytes = new Uint8Array(16);
  const nowMs = Date.now();
  bytes[0] = (nowMs / 0x10000000000) & 0xff;
  bytes[1] = (nowMs / 0x100000000) & 0xff;
  bytes[2] = (nowMs >> 24) & 0xff;
  bytes[3] = (nowMs >> 16) & 0xff;
  bytes[4] = (nowMs >> 8) & 0xff;
  bytes[5] = nowMs & 0xff;
  crypto.getRandomValues(bytes.subarray(6));
  // Base32 encode 128 bits → 26 chars.
  let bits = 0n;
  for (const b of bytes) bits = (bits << 8n) | BigInt(b);
  let out = "";
  for (let i = 25; i >= 0; i--) {
    const chunk = Number((bits >> BigInt(i * 5)) & 0x1fn);
    out += ALPHA[chunk];
  }
  // Take rightmost 26 chars (we shifted in 130 bits, drop top 2 alpha pad).
  return out.slice(-26);
}

/// 32-char URL-safe activation code. Crypto-strength random — we use
/// these in single-use magic links so brute-force is impractical
/// (32 × log2(64) = 192 bits).
export function activationCode(): string {
  const ALPHA =
    "ABCDEFGHJKMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789";
  const bytes = new Uint8Array(32);
  crypto.getRandomValues(bytes);
  let out = "";
  for (const b of bytes) out += ALPHA[b % ALPHA.length];
  return out;
}
