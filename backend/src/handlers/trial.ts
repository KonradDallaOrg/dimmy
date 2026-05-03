// POST /api/trial/start { email } — provision a 14-day trial license
// and email a magic link. Idempotent for the email: re-requesting
// returns a fresh activation code for the SAME license, not a fresh
// 14-day window. (Trial reset prevention — the property the PoC
// scenario #7 validates.)

import type { Env } from "../index";
import { json } from "../index";
import {
  audit,
  findActiveLicenseByEmail,
  findRecentUnconsumedActivationCode,
  insertActivationCode,
  insertLicense,
} from "../db";
import { activationCode, emailHash, ulid } from "../crypto";
import { sendActivationEmail } from "../email";

const TRIAL_VALIDITY_SECS = 14 * 86_400;
const ACTIVATION_TTL_SECS = 600; // 10 minutes
/// Magic-link dedup window: if an unconsumed code for THIS license was
/// minted within the last MAGIC_LINK_DEDUP_SECS, return the same one
/// instead of minting a new. Stops "I clicked Start trial twice" from
/// flooding the user's inbox with two different magic links.
/// 5 minutes ≈ half of ACTIVATION_TTL_SECS — long enough to catch a
/// double-click + page reload, short enough that a deliberate "send
/// me a fresh link" minutes later still gets a new one.
const MAGIC_LINK_DEDUP_SECS = 300;

export async function handleTrialStart(
  req: Request,
  env: Env,
  _ctx: ExecutionContext
): Promise<Response> {
  let body: { email?: unknown };
  try {
    body = await req.json();
  } catch {
    return json({ error: "invalid JSON body" }, 400);
  }
  const email =
    typeof body.email === "string" ? body.email.trim().toLowerCase() : "";
  if (!email || !email.includes("@")) {
    return json({ error: "email required" }, 400);
  }

  const eh = await emailHash(email);
  const now = Math.floor(Date.now() / 1000);

  // Find existing active license (trial or paid). Idempotent re-issuance.
  const existing = await findActiveLicenseByEmail(env.DB, eh);
  let licenseId: string;

  if (existing) {
    // Trial that already expired: tell the user to buy. Don't silently
    // re-issue a fresh trial (that'd be the easy bypass for the
    // "delete file → fresh trial" abuse pattern).
    if (existing.tier === "trial" && existing.valid_until < now) {
      await audit(
        env.DB,
        {
          event_type: "trial_start_rejected",
          email_hash: eh,
          details: { reason: "trial expired" },
        },
        now
      );
      return json(
        { error: "trial already used and expired — please purchase" },
        409
      );
    }
    licenseId = existing.license_id;
  } else {
    licenseId = ulid();
    await insertLicense(env.DB, {
      license_id: licenseId,
      email_hash: eh,
      tier: "trial",
      issued_at: now,
      valid_until: now + TRIAL_VALIDITY_SECS,
    });
    await audit(
      env.DB,
      { event_type: "trial_created", email_hash: eh, license_id: licenseId },
      now
    );
  }

  // Magic-link dedup: re-use a fresh unconsumed code if one was
  // minted in the last MAGIC_LINK_DEDUP_SECS for this license.
  const recent = await findRecentUnconsumedActivationCode(
    env.DB,
    licenseId,
    now - MAGIC_LINK_DEDUP_SECS
  );
  const code = recent?.code ?? activationCode();
  if (!recent) {
    await insertActivationCode(env.DB, {
      code,
      license_id: licenseId,
      created_at: now,
      expires_at: now + ACTIVATION_TTL_SECS,
    });
  }

  // HTTPS bridge URL — the Worker's GET /activate page auto-redirects
  // to dimmy://activate?code=… while presenting a paste-code fallback.
  // Sending the dimmy:// scheme directly in email breaks: Gmail and
  // most webmail strip non-http(s) schemes from <a href>, leaving the
  // button as un-clickable text.
  const magicLink = `${env.PUBLIC_URL}/activate?code=${encodeURIComponent(code)}`;

  // Send email (Resend) — falls back to console.log when no API key.
  await sendActivationEmail({
    to: email,
    magicLink,
    activationCode: code,
    tier: (existing?.tier ?? "trial") as
      | "trial"
      | "monthly"
      | "annual"
      | "lifetime",
    apiKey: env.RESEND_API_KEY ?? "",
    from: env.EMAIL_FROM,
  });

  return json({ magic_link: magicLink, code });
}
