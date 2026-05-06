// POST /api/account/delete — GDPR right-to-erasure endpoint.
//
// Two-step flow to prevent unauthenticated wipes:
//   1. POST { email }       → server emails an OTP magic-link
//                              (reuses the activation flow, with
//                              `intent: delete` in the audit log).
//   2. POST { email, code } → if code matches, we anonymise:
//                              - flip license status='deleted',
//                              - replace email_hash with placeholder
//                                'deleted-<ulid>' so old rows can no
//                                longer be looked up by email,
//                              - leave audit_log intact (we need the
//                                trail to prove we honoured the request).
//
// "Anonymise, don't drop" is the GDPR best practice for accounting /
// audit trails — see Recital 26 (anonymous data is outside GDPR scope).
//
// Devices and tokens become orphans (license_id still points but
// status='deleted' on the license means refresh fails). Background
// cleanup of orphan tokens is a future task.

import type { Env } from "../index";
import { json } from "../index";
import {
  audit,
  consumeActivationCode,
  findActivationCode,
  findActiveLicenseByEmail,
  insertActivationCode,
} from "../db";
import { activationCode, emailHash, ulid } from "../crypto";
import { sendActivationEmail } from "../email";

const DELETE_CODE_TTL_SECS = 600;

export async function handleAccountDelete(
  req: Request,
  env: Env,
  _ctx: ExecutionContext
): Promise<Response> {
  let body: { email?: unknown; code?: unknown };
  try {
    body = await req.json();
  } catch {
    return json({ error: "invalid JSON body" }, 400);
  }
  const email =
    typeof body.email === "string" ? body.email.trim().toLowerCase() : "";
  const code = typeof body.code === "string" ? body.code : "";
  if (!email || !email.includes("@")) {
    return json({ error: "email required" }, 400);
  }

  const eh = await emailHash(email);
  const now = Math.floor(Date.now() / 1000);
  const lic = await findActiveLicenseByEmail(env.DB, eh);
  if (!lic) {
    // Don't leak whether the email is on file. Always return success
    // for the OTP request step — the user gets the email if they have
    // an account, or nothing if they don't.
    if (!code) return json({ status: "if-exists, OTP sent" });
    return json({ error: "invalid code" }, 400);
  }

  // ── Step 1: no code yet → mint OTP, send email ──────────────────
  if (!code) {
    const otp = activationCode();
    await insertActivationCode(env.DB, {
      code: otp,
      license_id: lic.license_id,
      created_at: now,
      expires_at: now + DELETE_CODE_TTL_SECS,
    });
    const link = `${env.PUBLIC_URL.replace(/\/+$/, "")}/api/account/delete?code=${encodeURIComponent(
      otp
    )}`;
    // Reuse the activation email template — slight UX wart (subject
    // says "activate") but the magic-link flow is identical. A future
    // PR can split deletion-OTP into its own template.
    await sendActivationEmail({
      to: email,
      magicLink: link,
      activationCode: otp,
      tier: lic.tier,
      apiKey: env.RESEND_API_KEY ?? "",
      from: env.EMAIL_FROM,
    });
    await audit(
      env.DB,
      {
        event_type: "account_delete_otp_sent",
        email_hash: eh,
        license_id: lic.license_id,
      },
      now
    );
    return json({ status: "OTP sent" });
  }

  // ── Step 2: code provided → verify + anonymise ───────────────────
  const codeRow = await findActivationCode(env.DB, code);
  if (!codeRow) return json({ error: "invalid code" }, 400);
  if (codeRow.consumed_at !== null) return json({ error: "code already used" }, 409);
  if (codeRow.expires_at < now) return json({ error: "code expired" }, 409);
  if (codeRow.license_id !== lic.license_id) {
    return json({ error: "code does not match account" }, 400);
  }

  const claimed = await consumeActivationCode(env.DB, code, now);
  if (!claimed) return json({ error: "code already used" }, 409);

  const placeholderHash = `deleted-${ulid()}`;
  await env.DB.prepare(
    `UPDATE licenses SET status = 'deleted', email_hash = ?1
     WHERE license_id = ?2`
  )
    .bind(placeholderHash, lic.license_id)
    .run();

  await audit(
    env.DB,
    {
      event_type: "account_deleted",
      email_hash: placeholderHash, // post-anonymisation pointer
      license_id: lic.license_id,
      details: { original_email_hash_truncated: eh.slice(0, 8) },
    },
    now
  );

  return json({ status: "deleted" });
}
