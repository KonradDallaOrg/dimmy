// GET /activate?code=… — HTTPS landing page that bridges email-link
// clicks to the dimmy:// custom URL scheme.
//
// Why this exists:
//   Gmail / Outlook web strip non-http(s) URL schemes from <a href>
//   for security — so a `dimmy://activate…` link in email body renders
//   as un-clickable text. We send the user to this https://… page,
//   which immediately fires `window.location = "dimmy://…"`. The OS
//   prompts "Open with Dimmy?" and the activation completes in-app.
//
// HTTP 302 redirect to dimmy:// also doesn't work reliably — modern
// Chromium-family browsers block cross-scheme redirects from a server
// response. Only a same-page JS-driven navigation works in 2026.
//
// This endpoint is GET-only and does NOT consume the activation code —
// the actual /api/activate call still happens client-side once Dimmy
// receives the dimmy:// URL.

import type { Env } from "../index";

export async function handleActivateRedirect(
  req: Request,
  _env: Env,
  _ctx: ExecutionContext
): Promise<Response> {
  const url = new URL(req.url);
  const code = url.searchParams.get("code");
  if (!code || !/^[A-Za-z0-9]{8,64}$/.test(code)) {
    return new Response("Invalid or missing activation code.", {
      status: 400,
      headers: { "Content-Type": "text/plain; charset=utf-8" },
    });
  }
  const dimmyUrl = `dimmy://activate?code=${encodeURIComponent(code)}`;
  const html = renderRedirect(dimmyUrl, code);
  return new Response(html, {
    status: 200,
    headers: {
      "Content-Type": "text/html; charset=utf-8",
      // No-store: this page contains the (single-use) activation code
      // in the URL — don't let CDNs / browsers cache it.
      "Cache-Control": "no-store, max-age=0",
      "X-Frame-Options": "DENY",
      "Referrer-Policy": "no-referrer",
    },
  });
}

function renderRedirect(dimmyUrl: string, code: string): string {
  const safeUrl = escapeHtml(dimmyUrl);
  const safeCode = escapeHtml(code);
  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <meta name="robots" content="noindex,nofollow">
  <meta name="theme-color" content="#0a0a0f">
  <title>Activate Dimmy</title>
  <style>
    *, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }
    :root {
      --bg: #0a0a0f;
      --bg-glow: radial-gradient(1200px 600px at 50% -10%, rgba(120, 119, 198, 0.18), transparent 60%),
                 radial-gradient(900px 500px at 80% 110%, rgba(255, 122, 89, 0.12), transparent 60%);
      --fg: #f4f4f7;
      --fg-dim: rgba(244, 244, 247, 0.62);
      --fg-faint: rgba(244, 244, 247, 0.42);
      --card: rgba(255, 255, 255, 0.04);
      --card-border: rgba(255, 255, 255, 0.08);
      --code-bg: rgba(255, 255, 255, 0.06);
      --accent: #f4f4f7;
      --accent-fg: #0a0a0f;
    }
    html, body {
      height: 100%;
      background: var(--bg);
      color: var(--fg);
      font-family: -apple-system, BlinkMacSystemFont, "Inter", "Segoe UI", system-ui, sans-serif;
      -webkit-font-smoothing: antialiased;
      -moz-osx-font-smoothing: grayscale;
      font-feature-settings: "ss01", "cv11";
    }
    body {
      background-image: var(--bg-glow);
      background-attachment: fixed;
      display: flex;
      flex-direction: column;
      align-items: center;
      justify-content: center;
      padding: 24px;
      gap: 20px;
    }
    .brand {
      display: flex;
      align-items: center;
      gap: 10px;
      font-weight: 600;
      font-size: 14px;
      letter-spacing: -0.01em;
      color: var(--fg-dim);
      margin-bottom: 4px;
    }
    .brand-dot {
      width: 10px; height: 10px; border-radius: 50%;
      background: linear-gradient(135deg, #f4f4f7 0%, #c5c5d4 100%);
      box-shadow: 0 0 16px rgba(244, 244, 247, 0.6);
    }
    .card {
      width: 100%;
      max-width: 440px;
      background: var(--card);
      border: 1px solid var(--card-border);
      border-radius: 16px;
      padding: 32px 28px;
      backdrop-filter: blur(20px);
      -webkit-backdrop-filter: blur(20px);
    }
    h1 {
      font-size: 22px;
      font-weight: 600;
      letter-spacing: -0.02em;
      margin-bottom: 8px;
    }
    .lede {
      font-size: 14px;
      line-height: 1.55;
      color: var(--fg-dim);
      margin-bottom: 24px;
    }
    .status {
      display: flex;
      align-items: center;
      gap: 10px;
      font-size: 13px;
      color: var(--fg-dim);
      margin-bottom: 20px;
    }
    .spinner {
      width: 14px; height: 14px;
      border: 1.5px solid rgba(244, 244, 247, 0.2);
      border-top-color: var(--fg);
      border-radius: 50%;
      animation: spin 0.7s linear infinite;
    }
    .spinner.done {
      border: none;
      background: #4ade80;
      animation: none;
      position: relative;
    }
    .spinner.done::after {
      content: "";
      position: absolute;
      left: 4px; top: 1.5px;
      width: 4px; height: 8px;
      border: solid #0a0a0f;
      border-width: 0 1.5px 1.5px 0;
      transform: rotate(45deg);
    }
    @keyframes spin { to { transform: rotate(360deg); } }
    .btn {
      display: inline-flex;
      align-items: center;
      justify-content: center;
      width: 100%;
      padding: 12px 20px;
      background: var(--accent);
      color: var(--accent-fg);
      text-decoration: none;
      font-size: 14px;
      font-weight: 600;
      letter-spacing: -0.005em;
      border-radius: 10px;
      border: 0;
      cursor: pointer;
      transition: transform 0.08s ease, opacity 0.15s ease;
    }
    .btn:hover { transform: translateY(-1px); }
    .btn:active { transform: translateY(0); opacity: 0.9; }
    .divider {
      height: 1px;
      background: var(--card-border);
      margin: 24px 0 20px;
    }
    .fallback-label {
      font-size: 12px;
      color: var(--fg-faint);
      letter-spacing: 0.02em;
      text-transform: uppercase;
      margin-bottom: 10px;
    }
    .fallback-help {
      font-size: 13px;
      color: var(--fg-dim);
      line-height: 1.55;
      margin-bottom: 12px;
    }
    .fallback-help strong { color: var(--fg); font-weight: 600; }
    .code-row {
      display: flex;
      gap: 6px;
    }
    .code {
      flex: 1;
      background: var(--code-bg);
      border: 1px solid var(--card-border);
      padding: 11px 12px;
      border-radius: 8px;
      font-family: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
      font-size: 12.5px;
      letter-spacing: 0;
      user-select: all;
      word-break: break-all;
      color: var(--fg);
    }
    .copy-btn {
      padding: 0 14px;
      background: rgba(255, 255, 255, 0.06);
      color: var(--fg);
      border: 1px solid var(--card-border);
      border-radius: 8px;
      font-size: 12px;
      font-weight: 500;
      cursor: pointer;
      transition: background 0.12s ease;
      flex-shrink: 0;
    }
    .copy-btn:hover { background: rgba(255, 255, 255, 0.1); }
    .copy-btn.copied { background: #4ade80; color: #0a0a0f; border-color: #4ade80; }
    .meta {
      font-size: 11.5px;
      color: var(--fg-faint);
      margin-top: 14px;
      letter-spacing: 0.005em;
    }
    .footer {
      font-size: 11px;
      color: var(--fg-faint);
      margin-top: 8px;
    }
    @media (prefers-color-scheme: light) {
      :root {
        --bg: #fafaf7;
        --bg-glow: radial-gradient(1200px 600px at 50% -10%, rgba(120, 119, 198, 0.08), transparent 60%);
        --fg: #0a0a0f;
        --fg-dim: rgba(10, 10, 15, 0.65);
        --fg-faint: rgba(10, 10, 15, 0.42);
        --card: rgba(255, 255, 255, 0.7);
        --card-border: rgba(10, 10, 15, 0.08);
        --code-bg: rgba(10, 10, 15, 0.04);
        --accent: #0a0a0f;
        --accent-fg: #fafaf7;
      }
      .copy-btn { background: rgba(10, 10, 15, 0.05); }
      .copy-btn:hover { background: rgba(10, 10, 15, 0.09); }
    }
  </style>
</head>
<body>
  <div class="brand">
    <span class="brand-dot"></span>
    <span>Dimmy</span>
  </div>
  <div class="card">
    <h1>Activating your license</h1>
    <p class="lede">Your browser is opening Dimmy. If you see a permission prompt, click <strong>Open</strong>.</p>
    <div class="status" id="status">
      <span class="spinner" id="spinner"></span>
      <span id="status-text">Opening Dimmy…</span>
    </div>
    <a id="open" class="btn" href="${safeUrl}">Open Dimmy</a>
    <div class="divider"></div>
    <p class="fallback-label">Doesn't open?</p>
    <p class="fallback-help">Make sure Dimmy is installed, then go to <strong>Settings → License</strong> and paste this code:</p>
    <div class="code-row">
      <span class="code" id="code">${safeCode}</span>
      <button class="copy-btn" id="copy-btn" type="button">Copy</button>
    </div>
    <p class="meta">Single-use code. Expires in 10 minutes.</p>
  </div>
  <p class="footer">dimmy.app</p>

  <script>
    var dimmyUrl = ${JSON.stringify(dimmyUrl)};
    // Kick off the redirect after the card has had a beat to render.
    // 200ms is short enough to feel instant but long enough for the
    // status indicator to be visible if the redirect succeeds.
    setTimeout(function() { window.location.href = dimmyUrl; }, 200);
    // After ~1.5s assume the OS prompted and Dimmy took over. Flip
    // the status to a soft success state so a user who returns to
    // the tab sees feedback instead of a forever-spinner.
    setTimeout(function() {
      var s = document.getElementById('spinner');
      var t = document.getElementById('status-text');
      if (s) s.classList.add('done');
      if (t) t.textContent = 'Activation sent to Dimmy';
    }, 1500);

    var btn = document.getElementById('copy-btn');
    if (btn && navigator.clipboard) {
      btn.addEventListener('click', function() {
        navigator.clipboard.writeText(${JSON.stringify(code)}).then(function() {
          btn.textContent = 'Copied';
          btn.classList.add('copied');
          setTimeout(function() { btn.textContent = 'Copy'; btn.classList.remove('copied'); }, 1600);
        }).catch(function() {});
      });
    }
  </script>
</body>
</html>`;
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}
