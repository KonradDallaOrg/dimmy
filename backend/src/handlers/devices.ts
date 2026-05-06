// POST /api/devices/list { token }
// POST /api/devices/deactivate { token, device_id? }
//
// Auth: caller proves identity by sending its current Ed25519-signed token.
// We verify the signature against DIMMY_LICENSE_PUBKEY, then look up the
// license by `claims.lid`. Any active device on the license can list all
// devices and deactivate any other device under the same license — this
// lets a user free up a device slot from another machine when they hit
// the 5-device limit.

import type { Env } from "../index";
import { json } from "../index";
import {
  audit,
  deactivateDeviceById,
  findLicenseById,
  listActiveDevices,
} from "../db";
import { verifyTokenWithPub, type Claims } from "../crypto";

export async function handleDevicesList(
  req: Request,
  env: Env,
  _ctx: ExecutionContext
): Promise<Response> {
  const claims = await authenticateOrFail(req, env);
  if (claims instanceof Response) return claims;

  const lic = await findLicenseById(env.DB, claims.lid);
  if (!lic) return json({ error: "license not found" }, 404);

  const rows = await listActiveDevices(env.DB, lic.license_id);
  const devices = rows.map((d) => ({
    device_id: d.device_id,
    label: d.device_label,
    issued_at: d.issued_at,
    last_seen: d.last_seen,
    status: d.status,
    is_self: d.device_id === claims.did,
  }));

  return json({
    license_id: lic.license_id,
    tier: lic.tier,
    max_devices: lic.max_devices,
    devices,
  });
}

export async function handleDeviceDeactivate(
  req: Request,
  env: Env,
  _ctx: ExecutionContext
): Promise<Response> {
  let body: { token?: unknown; device_id?: unknown };
  try {
    body = await req.json();
  } catch {
    return json({ error: "invalid JSON body" }, 400);
  }
  const claims = await authenticateClaim(body, env);
  if (claims instanceof Response) return claims;

  const target =
    typeof body.device_id === "string" && body.device_id.length > 0
      ? body.device_id
      : claims.did; // self-sign-out when no explicit device_id

  const lic = await findLicenseById(env.DB, claims.lid);
  if (!lic) return json({ error: "license not found" }, 404);

  const changed = await deactivateDeviceById(env.DB, target, lic.license_id);
  if (changed === 0) {
    // Either device doesn't belong to this license, or already revoked.
    // Don't disclose which — same response either way.
    return json({ error: "device not found on this license" }, 404);
  }

  const now = Math.floor(Date.now() / 1000);
  await audit(
    env.DB,
    {
      event_type: "device_deactivated",
      email_hash: lic.email_hash,
      license_id: lic.license_id,
      details: {
        device_id: target,
        by_device_id: claims.did,
        is_self: target === claims.did,
      },
    },
    now
  );

  return json({ ok: true, device_id: target });
}

// ── helpers ──────────────────────────────────────────────────────────

async function authenticateOrFail(
  req: Request,
  env: Env
): Promise<Claims | Response> {
  let body: { token?: unknown };
  try {
    body = await req.json();
  } catch {
    return json({ error: "invalid JSON body" }, 400);
  }
  return authenticateClaim(body, env);
}

async function authenticateClaim(
  body: { token?: unknown },
  env: Env
): Promise<Claims | Response> {
  const token = typeof body.token === "string" ? body.token : "";
  if (!token) return json({ error: "token required" }, 400);
  try {
    return await verifyTokenWithPub(token, env.DIMMY_LICENSE_PUBKEY);
  } catch (err) {
    const msg = err instanceof Error ? err.message : "unknown";
    return json({ error: `invalid token: ${msg}` }, 400);
  }
}
