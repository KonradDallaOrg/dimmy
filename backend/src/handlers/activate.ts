// GET /api/activate?code=…&device_label=… — exchange an activation
// code for a signed token. Enforces the 5-device limit per license;
// when exceeded, returns 429 with the active devices so the UI can
// offer a "deactivate one to continue" picker.

import type { Env } from "../index";
import { json } from "../index";
import {
  audit,
  consumeActivationCode,
  countActiveDevices,
  findActivationCode,
  findLicenseById,
  insertDevice,
  listActiveDevices,
} from "../db";
import { signToken, ulid, type Claims } from "../crypto";

// Tier → max-offline-days mapping. Keep in sync with Rust
// `Tier::default_max_offline_days` in core/src/license.rs.
const MAX_OFFLINE: Record<"trial" | "annual" | "3year", number> = {
  trial: 30,
  annual: 30,
  "3year": 1095,
};

export async function handleActivate(
  req: Request,
  env: Env,
  _ctx: ExecutionContext
): Promise<Response> {
  const url = new URL(req.url);
  const code = url.searchParams.get("code");
  const deviceLabel =
    url.searchParams.get("device_label") || "Unknown device";

  if (!code) return json({ error: "code required" }, 400);
  if (!deviceLabel.trim()) return json({ error: "device_label required" }, 400);

  const now = Math.floor(Date.now() / 1000);

  const codeRow = await findActivationCode(env.DB, code);
  if (!codeRow) return json({ error: "unknown activation code" }, 404);
  if (codeRow.consumed_at !== null)
    return json({ error: "activation code already used" }, 409);
  if (codeRow.expires_at < now)
    return json({ error: "activation code expired" }, 409);

  const lic = await findLicenseById(env.DB, codeRow.license_id);
  if (!lic) return json({ error: "license not found" }, 404);
  if (lic.status !== "active")
    return json({ error: "license suspended" }, 409);

  const activeCount = await countActiveDevices(env.DB, lic.license_id);
  if (activeCount >= lic.max_devices) {
    const devices = await listActiveDevices(env.DB, lic.license_id);
    return json(
      {
        error: `device limit ${lic.max_devices} reached — deactivate one to continue`,
        active_devices: devices.map((d) => ({
          device_id: d.device_id,
          label: d.device_label,
          last_seen: d.last_seen,
        })),
      },
      429
    );
  }

  // Atomic claim of the activation code. If another concurrent activate
  // beat us, consume returns false and we error out cleanly.
  const claimed = await consumeActivationCode(env.DB, code, now);
  if (!claimed) return json({ error: "activation code already used" }, 409);

  const deviceId = ulid();
  await insertDevice(env.DB, {
    device_id: deviceId,
    license_id: lic.license_id,
    device_label: deviceLabel,
    now,
  });

  const claims: Claims = {
    v: 1,
    lid: lic.license_id,
    eh: lic.email_hash,
    tier: lic.tier,
    iat: now,
    exp: lic.valid_until,
    max_offline: MAX_OFFLINE[lic.tier],
    did: deviceId,
    scope: ["cloud", "updates"],
  };
  const token = await signToken(claims, env.DIMMY_LICENSE_PRIVKEY);

  await audit(
    env.DB,
    {
      event_type: "device_activated",
      email_hash: lic.email_hash,
      license_id: lic.license_id,
      details: { device_id: deviceId, label: deviceLabel },
    },
    now
  );

  return json({ token });
}
