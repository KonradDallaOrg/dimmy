// Resend integration — sends activation magic links.
//
// Mirror what the PoC server prints to stdout. Same magic_link URL,
// just delivered via SMTP instead of dev-console. The client never
// knows the difference: both paths land at /api/activate?code=…
//
// Templates are inline (no separate template engine) — for MVP this
// is plenty, and it keeps the Worker bundle small. If we add more
// email types later (renewal nudge, refund confirmation), revisit.

interface ResendPayload {
  from: string;
  to: string[];
  subject: string;
  html: string;
  text: string;
}

/// Send a magic-link email via Resend.
export async function sendActivationEmail(opts: {
  to: string;
  magicLink: string;
  activationCode: string;
  tier: "trial" | "monthly" | "annual" | "lifetime";
  apiKey: string;
  from: string;
}): Promise<void> {
  if (!opts.apiKey) {
    // Dev fallback: print to console (Workers logs) the same way the
    // PoC server printed to stdout. Keeps `wrangler dev` usable
    // without setting up a real Resend account.
    console.log(`[email/dev] to=${opts.to} link=${opts.magicLink}`);
    return;
  }

  const { subject, html, text } = renderActivation(opts);
  const payload: ResendPayload = {
    from: opts.from,
    to: [opts.to],
    subject,
    html,
    text,
  };

  const resp = await fetch("https://api.resend.com/emails", {
    method: "POST",
    headers: {
      Authorization: `Bearer ${opts.apiKey}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(payload),
  });
  if (!resp.ok) {
    const body = await resp.text();
    throw new Error(`Resend ${resp.status}: ${body.slice(0, 200)}`);
  }
}

function renderActivation(opts: {
  magicLink: string;
  activationCode: string;
  tier: "trial" | "monthly" | "annual" | "lifetime";
}): { subject: string; html: string; text: string } {
  const tierName = (() => {
    switch (opts.tier) {
      case "trial":
        return "your 14-day free trial";
      case "monthly":
        return "your monthly subscription";
      case "annual":
        return "your annual subscription";
      case "lifetime":
        return "your lifetime license";
    }
  })();

  // Unique-ish subject per email — Gmail (and Resend's own dedup heuristic)
  // were silently dropping repeated identical-subject sends to the same
  // recipient within a short window, observed live on 2026-05-04 when an
  // in-place lifetime upgrade's magic link never reached the inbox after
  // two prior identical-subject emails for the same license. Embed the
  // last 6 chars of the activation code so each email gets a distinct
  // subject string while staying readable. The code is single-use anyway,
  // so this leaks nothing useful to anyone reading the inbox preview.
  const codeTag = opts.activationCode.slice(-6);
  const subject =
    opts.tier === "trial"
      ? `Activate your Dimmy trial · ${codeTag}`
      : `Activate your Dimmy ${opts.tier} license · ${codeTag}`;

  // Plain-text version — robust against email clients that strip HTML.
  // Activation code is shown separately so users on a different device
  // (e.g. read email on phone, install on laptop) can copy-paste it
  // into Settings → License instead of clicking the link.
  const text = `Welcome to Dimmy.

Click to activate ${tierName}:
${opts.magicLink}

Or paste this activation code in Dimmy → Settings → License:

  ${opts.activationCode}

The link / code is single-use and expires in 10 minutes. If it
expired, just request a new one from the app.

— The Dimmy team
`;

  // Minimal HTML — single-column, monospace code block, system fonts.
  // Lots of email clients mangle CSS; we keep styling inline + simple.
  const html = `<!doctype html>
<html><body style="font-family:-apple-system,BlinkMacSystemFont,Segoe UI,sans-serif;color:#0f172a;max-width:480px;margin:0 auto;padding:24px;line-height:1.5">
  <h1 style="font-size:20px;margin:0 0 16px">Welcome to Dimmy</h1>
  <p>Click to activate ${tierName}:</p>
  <p style="margin:24px 0">
    <a href="${escapeHtml(opts.magicLink)}"
       style="display:inline-block;background:#0f172a;color:#fff;text-decoration:none;padding:10px 18px;border-radius:6px;font-weight:600">
      Open in Dimmy
    </a>
  </p>
  <p style="color:#64748b;font-size:14px">Or paste this code in <strong>Dimmy → Settings → License</strong>:</p>
  <pre style="background:#f1f5f9;padding:12px 16px;border-radius:6px;font-family:ui-monospace,Menlo,Consolas,monospace;font-size:14px;letter-spacing:.5px;user-select:all">${escapeHtml(opts.activationCode)}</pre>
  <p style="color:#94a3b8;font-size:12px;margin-top:32px">The link/code expires in 10 minutes. Request a new one from the app if you need to.</p>
</body></html>`;

  return { subject, html, text };
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}
