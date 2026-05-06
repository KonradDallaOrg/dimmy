// GET /api/license/status?email=… or ?license_id=… — debug introspection.
//
// Lists licenses + active devices for the given identifier. Read-only,
// no PII leakage (we hash the email before lookup, and the response
// only contains email_hash). Mirrors the PoC's debug endpoint.
//
// In production, gate this behind an admin token if you don't want it
// publicly accessible. For initial rollout it's open — the email_hash
// is opaque (not the email itself) and reveals only license existence
// + device count, which is acceptable.

import type { Env } from "../index";
import { json } from "../index";
import { emailHash } from "../crypto";
import { findLicenseById, listActiveDevices } from "../db";

export async function handleStatusDebug(
  req: Request,
  env: Env,
  _ctx: ExecutionContext
): Promise<Response> {
  const url = new URL(req.url);
  const email = url.searchParams.get("email");
  const licenseId = url.searchParams.get("license_id");

  if (!email && !licenseId) {
    return json({ error: "email or license_id required" }, 400);
  }

  const licenses = email
    ? await env.DB.prepare(
        `SELECT license_id, email_hash, tier, issued_at, valid_until,
                max_devices, status, stripe_session_id, stripe_customer_id
         FROM licenses WHERE email_hash = ?1`
      )
        .bind(await emailHash(email))
        .all<{
          license_id: string;
          email_hash: string;
          tier: string;
          issued_at: number;
          valid_until: number;
          max_devices: number;
          status: string;
        }>()
        .then((r) => r.results)
    : licenseId
    ? await findLicenseById(env.DB, licenseId).then((r) => (r ? [r] : []))
    : [];

  const out = await Promise.all(
    licenses.map(async (lic) => {
      const devices = await listActiveDevices(env.DB, lic.license_id);
      return {
        license_id: lic.license_id,
        email_hash: lic.email_hash,
        tier: lic.tier,
        issued_at: lic.issued_at,
        valid_until: lic.valid_until,
        status: lic.status,
        max_devices: lic.max_devices,
        devices: devices.map((d) => ({
          device_id: d.device_id,
          label: d.device_label,
          issued_at: d.issued_at,
          last_seen: d.last_seen,
          status: d.status,
        })),
      };
    })
  );

  return json(out);
}
