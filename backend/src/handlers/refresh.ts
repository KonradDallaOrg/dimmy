// POST /api/refresh { token } — verify the existing token, bump the
// device's last_seen, return a re-issued token (same lid + did,
// fresh iat). Client calls this every ~24h while online so the
// last_online_check sidecar stays fresh and we can detect cancellations
// / chargebacks within the soft offline grace window.

import type { Env } from "../index";
import { json } from "../index";
import {
  bumpDeviceLastSeen,
  findDeviceStatus,
  findLicenseById,
} from "../db";
import { signToken, verifyTokenWithPub, type Claims } from "../crypto";

const MAX_OFFLINE: Record<"trial" | "annual" | "3year", number> = {
  trial: 30,
  annual: 30,
  "3year": 1095,
};

export async function handleRefresh(
  req: Request,
  env: Env,
  _ctx: ExecutionContext
): Promise<Response> {
  let body: { token?: unknown };
  try {
    body = await req.json();
  } catch {
    return json({ error: "invalid JSON body" }, 400);
  }
  const token = typeof body.token === "string" ? body.token : "";
  if (!token) return json({ error: "token required" }, 400);

  let claims: Claims;
  try {
    claims = await verifyTokenWithPub(token, env.DIMMY_LICENSE_PUBKEY);
  } catch (err) {
    const msg = err instanceof Error ? err.message : "unknown";
    return json({ error: `invalid token: ${msg}` }, 400);
  }

  const lic = await findLicenseById(env.DB, claims.lid);
  if (!lic) return json({ error: "license not found" }, 404);
  if (lic.status !== "active")
    return json({ error: "license suspended" }, 409);

  const deviceStatus = await findDeviceStatus(env.DB, claims.did, lic.license_id);
  if (deviceStatus === null) return json({ error: "device not found" }, 404);
  if (deviceStatus !== "active")
    return json({ error: "device deactivated" }, 409);

  const now = Math.floor(Date.now() / 1000);
  await bumpDeviceLastSeen(env.DB, claims.did, now);

  const newClaims: Claims = {
    v: 1,
    lid: lic.license_id,
    eh: lic.email_hash,
    tier: lic.tier,
    iat: now,
    exp: lic.valid_until,
    max_offline: MAX_OFFLINE[lic.tier],
    did: claims.did,
    scope: claims.scope,
  };
  const newToken = await signToken(newClaims, env.DIMMY_LICENSE_PRIVKEY);
  return json({ token: newToken });
}
