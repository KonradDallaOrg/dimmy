import { describe, expect, test } from "vitest";
import {
  activationCode,
  b64urlDecode,
  b64urlEncode,
  emailHash,
  sha256Hex,
  signToken,
  ulid,
  verifyTokenWithPub,
  type Claims,
} from "../src/crypto";

// Dev keypair generated once for the test suite. Public-only constants
// so we never accidentally commit a "real" privkey here.
const TEST_PRIVKEY_B64 = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

describe("base64url", () => {
  test("roundtrip preserves bytes", () => {
    const original = new Uint8Array([0, 1, 2, 250, 251, 252, 253, 254, 255]);
    const encoded = b64urlEncode(original);
    expect(encoded).not.toContain("=");
    expect(encoded).not.toContain("+");
    expect(encoded).not.toContain("/");
    const decoded = b64urlDecode(encoded);
    expect(Array.from(decoded)).toEqual(Array.from(original));
  });

  test("decode tolerates length not multiple of 4", () => {
    expect(b64urlDecode("YQ").length).toBe(1); // 'a'
    expect(b64urlDecode("YWI").length).toBe(2); // 'ab'
    expect(b64urlDecode("YWJj").length).toBe(3); // 'abc'
  });
});

describe("sha256 + email_hash", () => {
  test("emailHash is normalised lowercase + trimmed", async () => {
    const a = await emailHash("Alice@Example.com");
    const b = await emailHash("alice@example.com");
    const c = await emailHash("  alice@example.com  ");
    expect(a).toBe(b);
    expect(b).toBe(c);
    expect(a.length).toBe(64); // hex of 32-byte SHA-256
  });

  test("sha256Hex matches a known vector", async () => {
    // sha256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
    const h = await sha256Hex("abc");
    expect(h).toBe(
      "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
  });
});

describe("ulid + activationCode", () => {
  test("ulid yields 26-char Crockford base32", () => {
    const id = ulid();
    expect(id.length).toBe(26);
    expect(/^[0-9A-HJKMNP-TV-Z]{26}$/.test(id)).toBe(true);
  });

  test("ulid uniqueness over 1k iterations", () => {
    const seen = new Set<string>();
    for (let i = 0; i < 1000; i++) seen.add(ulid());
    expect(seen.size).toBe(1000);
  });

  test("activationCode is 32 chars from the URL-safe alphabet", () => {
    // Same alphabet as activationCode() in src/crypto.ts. Avoids
    // ambiguous chars (0/O, 1/I/l) so codes are clean to dictate.
    const ALPHABET =
      "ABCDEFGHJKMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789";
    const c = activationCode();
    expect(c.length).toBe(32);
    for (const ch of c) {
      expect(ALPHABET.includes(ch)).toBe(true);
    }
  });

  test("activationCode entropy — 1k samples all distinct", () => {
    const seen = new Set<string>();
    for (let i = 0; i < 1000; i++) seen.add(activationCode());
    expect(seen.size).toBe(1000);
  });
});

describe("token sign + verify", () => {
  // Generate a fresh Ed25519 keypair using Node's webcrypto API. We
  // export the raw 32-byte seed (privkey) and 32-byte public point
  // and feed them to our own signToken / verifyTokenWithPub helpers.
  async function makeKeypair(): Promise<{ priv: string; pub: string }> {
    const kp = (await crypto.subtle.generateKey(
      { name: "Ed25519" } as EcKeyGenParams,
      true,
      ["sign", "verify"]
    )) as CryptoKeyPair;
    const pkcs8 = new Uint8Array(
      await crypto.subtle.exportKey("pkcs8", kp.privateKey)
    );
    // PKCS#8 prefix is 16 bytes (matches importEd25519PrivateKey).
    const seed = pkcs8.slice(16, 48);
    const spki = new Uint8Array(
      await crypto.subtle.exportKey("spki", kp.publicKey)
    );
    // SPKI prefix is 12 bytes.
    const pub = spki.slice(12, 44);
    return { priv: b64urlEncode(seed), pub: b64urlEncode(pub) };
  }

  test("signed token round-trips through verify", async () => {
    const { priv, pub } = await makeKeypair();
    const claims: Claims = {
      v: 1,
      lid: "01ABCDEFGHJKMNPQRSTVWXYZ00",
      eh: "abc",
      tier: "monthly",
      iat: 1_700_000_000,
      exp: 1_700_000_000 + 31 * 86400,
      max_offline: 14,
      did: "01DEVICEABCDEFGHJKMNPQRST0",
      scope: ["managed_stt", "managed_llm"],
    };
    const token = await signToken(claims, priv);
    expect(token.split(".").length).toBe(3);

    const back = await verifyTokenWithPub(token, pub);
    expect(back.lid).toBe(claims.lid);
    expect(back.tier).toBe("monthly");
    expect(back.exp).toBe(claims.exp);
    expect(back.scope).toEqual(claims.scope);
  });

  test("verify rejects tampered payload", async () => {
    const { priv, pub } = await makeKeypair();
    const claims: Claims = {
      v: 1,
      lid: "01ABC",
      eh: "x",
      tier: "lifetime",
      iat: 1,
      exp: 2,
      max_offline: 1,
      did: "01XYZ",
      scope: [],
    };
    const token = await signToken(claims, priv);
    const [h, p, s] = token.split(".");
    // Flip a byte in the payload — sig no longer matches.
    const pBytes = b64urlDecode(p);
    pBytes[0] ^= 0xff;
    const tampered = `${h}.${b64urlEncode(pBytes)}.${s}`;
    await expect(verifyTokenWithPub(tampered, pub)).rejects.toThrow(
      /signature verify/
    );
  });

  test("verify rejects under wrong public key", async () => {
    const { priv } = await makeKeypair();
    const { pub: otherPub } = await makeKeypair();
    const claims: Claims = {
      v: 1,
      lid: "01A",
      eh: "x",
      tier: "annual",
      iat: 1,
      exp: 2,
      max_offline: 1,
      did: "01B",
      scope: [],
    };
    const token = await signToken(claims, priv);
    await expect(verifyTokenWithPub(token, otherPub)).rejects.toThrow(
      /signature verify/
    );
  });

  test("verify rejects unknown schema version", async () => {
    const { priv, pub } = await makeKeypair();
    // Bypass the claims type to mint a v=2 token.
    const claims = {
      v: 2 as 1, // intentional mismatch — tests reject on parse
      lid: "01A",
      eh: "x",
      tier: "annual" as const,
      iat: 1,
      exp: 2,
      max_offline: 1,
      did: "01B",
      scope: [] as string[],
    };
    const token = await signToken(claims as Claims, priv);
    await expect(verifyTokenWithPub(token, pub)).rejects.toThrow(
      /unsupported schema/
    );
  });

  test("verify rejects malformed token", async () => {
    const { pub } = await makeKeypair();
    await expect(verifyTokenWithPub("a.b", pub)).rejects.toThrow();
    await expect(verifyTokenWithPub("a.b.c.d", pub)).rejects.toThrow();
  });

  test("priv key length is asserted", async () => {
    const claims: Claims = {
      v: 1,
      lid: "x",
      eh: "y",
      tier: "trial",
      iat: 1,
      exp: 2,
      max_offline: 1,
      did: "z",
      scope: [],
    };
    // Wrong-length privkey (3 bytes b64-encoded).
    await expect(signToken(claims, "AAAA")).rejects.toThrow(/32 bytes/);
    // The all-zero placeholder is 32 bytes of zero — a degenerate but
    // valid Ed25519 seed; no error expected here.
    await expect(signToken(claims, TEST_PRIVKEY_B64)).resolves.toBeDefined();
  });
});
