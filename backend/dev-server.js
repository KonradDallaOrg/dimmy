#!/usr/bin/env node
// Dimmy licensing — local dev mock server.
// Replaces the Rust licensing_server during development. 100% Node built-ins
// (no npm install). Same wire contract as the Cloudflare Worker so the
// Rust client (license.rs) talks to either with no code changes.
//
// Boot:  node backend/dev-server.js
// Stop:  Ctrl-C
// State: backend/.dev-data/state.json (gitignored)
//        Override via DIMMY_LICENSING_DATA / DIMMY_LICENSING_PORT.

import http from 'node:http';
import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const DATA_DIR   = process.env.DIMMY_LICENSING_DATA   || path.join(__dirname, '.dev-data');
const STATE_FILE = path.join(DATA_DIR, 'state.json');
const PORT       = Number(process.env.DIMMY_LICENSING_PORT || 8787);
const PUBLIC_URL = process.env.DIMMY_LICENSING_PUBLIC_URL || `http://127.0.0.1:${PORT}`;

const TIER_VALIDITY  = { trial: 14*86400, annual: 365*86400, '3year': 1095*86400 };
const TIER_MAX_OFFLINE = { trial: 30, annual: 30, '3year': 1095 };
const ACTIVATION_TTL = 600; // 10 min

// Capability-based scopes. Server-driven so we can change tier→scope
// mapping without a client release. Trial gets the full vetrina.
const SCOPES = {
    MANAGED_STT:    'managed_stt',
    MANAGED_LLM:    'managed_llm',
    AUTO_UPDATE:    'auto_update',
    HISTORY_SYNC:   'history_sync',
    PREMIUM_STYLES: 'premium_styles',
};
const SCOPES_FOR_TIER = {
    trial:   [SCOPES.MANAGED_STT, SCOPES.MANAGED_LLM, SCOPES.AUTO_UPDATE, SCOPES.HISTORY_SYNC, SCOPES.PREMIUM_STYLES],
    annual:  [SCOPES.MANAGED_STT, SCOPES.MANAGED_LLM, SCOPES.AUTO_UPDATE, SCOPES.PREMIUM_STYLES],
    '3year': [SCOPES.MANAGED_STT, SCOPES.MANAGED_LLM, SCOPES.AUTO_UPDATE, SCOPES.HISTORY_SYNC, SCOPES.PREMIUM_STYLES],
};

fs.mkdirSync(DATA_DIR, { recursive: true });
let state = loadState();
saveState();

function loadState() {
    if (fs.existsSync(STATE_FILE)) return JSON.parse(fs.readFileSync(STATE_FILE, 'utf-8'));
    const { publicKey, privateKey } = crypto.generateKeyPairSync('ed25519');
    return {
        key: { seedB64: privateKey.export({ format: 'jwk' }).d,
               pubB64:  publicKey.export({ format: 'jwk' }).x },
        licenses: [], devices: [], codes: [],
    };
}
function saveState() { fs.writeFileSync(STATE_FILE, JSON.stringify(state, null, 2)); }

const privKey = () => crypto.createPrivateKey({
    key: { kty: 'OKP', crv: 'Ed25519', d: state.key.seedB64, x: state.key.pubB64 }, format: 'jwk',
});
const pubKey  = () => crypto.createPublicKey({
    key: { kty: 'OKP', crv: 'Ed25519', x: state.key.pubB64 }, format: 'jwk',
});

const b64u   = (b) => Buffer.from(b).toString('base64url');
const ub64u  = (s) => Buffer.from(s, 'base64url');
const sha256 = (s) => crypto.createHash('sha256').update(s).digest('hex');
const eHash  = (e) => sha256(e.trim().toLowerCase());
const now    = () => Math.floor(Date.now() / 1000);

function ulid() {
    const ALPHA = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';
    const buf = Buffer.alloc(16);
    buf.writeUIntBE(Date.now(), 0, 6);
    crypto.randomFillSync(buf, 6, 10);
    let out = '', bits = 0n, n = 0, i = 0;
    while (i < 16 || n >= 5) {
        if (n < 5 && i < 16) { bits = (bits << 8n) | BigInt(buf[i]); n += 8; i++; }
        n -= 5;
        out += ALPHA[Number((bits >> BigInt(n)) & 0x1Fn)];
    }
    return out;
}

function activationCode() {
    const CHARS = 'ABCDEFGHJKMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789';
    return [...crypto.randomBytes(32)].map(b => CHARS[b % CHARS.length]).join('');
}

function signClaims(claims) {
    const header  = b64u(JSON.stringify({ alg: 'EdDSA', typ: 'DLT' }));
    const payload = b64u(JSON.stringify(claims));
    const sig     = crypto.sign(null, Buffer.from(`${header}.${payload}`), privKey());
    return `${header}.${payload}.${b64u(sig)}`;
}

function verifyToken(token) {
    const parts = token.split('.');
    if (parts.length !== 3) throw new Error('expected 3 segments');
    const ok = crypto.verify(null, Buffer.from(`${parts[0]}.${parts[1]}`), pubKey(), ub64u(parts[2]));
    if (!ok) throw new Error('signature mismatch');
    return JSON.parse(ub64u(parts[1]).toString());
}

function mintCode(license_id) {
    const code = activationCode();
    state.codes.push({ code, license_id, created_at: now(), expires_at: now() + ACTIVATION_TTL, consumed_at: null });
    saveState();
    return code;
}

const log = (msg) => console.log(`[${new Date().toISOString().slice(11, 19)}] ${msg}`);
const ok  = (body) => ({ status: 200, body });
const err = (status, message) => ({ status, body: { error: message } });

// ── Handlers ────────────────────────────────────────────────────────

const handlers = {
    'GET /api/health': () => ok({ status: 'ok' }),

    'POST /api/trial/start': ({ body }) => {
        const email = (body.email || '').trim().toLowerCase();
        if (!email || !email.includes('@')) return err(400, 'email required');
        const eh = eHash(email);
        let lic = state.licenses.filter(l => l.email_hash === eh && l.status === 'active')
                                .sort((a, b) => b.issued_at - a.issued_at)[0];
        if (lic && lic.tier === 'trial' && lic.valid_until < now()) {
            return err(409, 'trial already used and expired — please purchase');
        }
        if (!lic) {
            lic = { license_id: ulid(), email_hash: eh, tier: 'trial',
                    issued_at: now(), valid_until: now() + TIER_VALIDITY.trial,
                    max_devices: 5, status: 'active' };
            state.licenses.push(lic);
        }
        const code = mintCode(lic.license_id);
        const magic = `dimmy://activate?code=${code}`;
        log(`[trial-start]   ${email} → ${magic}`);
        return ok({ magic_link: magic, code });
    },

    'GET /api/activate': ({ query }) => {
        const code = query.code;
        const label = (query.device_label || 'Unknown device').trim() || 'Unknown device';
        if (!code) return err(400, 'code required');
        const ac = state.codes.find(c => c.code === code);
        if (!ac) return err(404, 'unknown activation code');
        if (ac.consumed_at) return err(409, 'activation code already used');
        if (ac.expires_at < now()) return err(409, 'activation code expired');
        const lic = state.licenses.find(l => l.license_id === ac.license_id);
        if (!lic || lic.status !== 'active') return err(409, 'license suspended or missing');
        const active = state.devices.filter(d => d.license_id === lic.license_id && d.status === 'active').length;
        if (active >= lic.max_devices) return err(429, `device limit ${lic.max_devices} reached — deactivate one to continue`);
        const did = ulid();
        ac.consumed_at = now();
        state.devices.push({ device_id: did, license_id: lic.license_id, device_label: label,
                              issued_at: now(), last_seen: now(), status: 'active' });
        saveState();
        const claims = { v: 1, lid: lic.license_id, eh: lic.email_hash, tier: lic.tier,
                          iat: now(), exp: lic.valid_until, max_offline: TIER_MAX_OFFLINE[lic.tier],
                          did, scope: SCOPES_FOR_TIER[lic.tier] || [] };
        log(`[activate]      ${lic.tier} → ${label} (${did.slice(0, 8)}…) scopes=${claims.scope.join(',')}`);
        return ok({ token: signClaims(claims) });
    },

    'POST /api/refresh': ({ body }) => {
        if (!body.token) return err(400, 'token required');
        let claims;
        try { claims = verifyToken(body.token); } catch (e) { return err(400, `invalid token: ${e.message}`); }
        const lic = state.licenses.find(l => l.license_id === claims.lid);
        if (!lic || lic.status !== 'active') return err(409, 'license suspended or missing');
        const dev = state.devices.find(d => d.device_id === claims.did && d.license_id === lic.license_id);
        if (!dev) return err(404, 'device not found');
        if (dev.status !== 'active') return err(409, 'device deactivated');
        dev.last_seen = now();
        saveState();
        const fresh = { ...claims, iat: now(), exp: lic.valid_until };
        log(`[refresh]       ${lic.tier} (${claims.did.slice(0, 8)}…) bumped`);
        return ok({ token: signClaims(fresh) });
    },

    'POST /api/license/issue': ({ body }) => {
        const email = (body.email || '').trim().toLowerCase();
        const tier = body.tier;
        if (!email || !email.includes('@')) return err(400, 'email required');
        if (tier !== 'annual' && tier !== '3year') return err(400, '/issue is for paid tiers');
        const lic = { license_id: ulid(), email_hash: eHash(email), tier,
                       issued_at: now(), valid_until: now() + TIER_VALIDITY[tier],
                       max_devices: 5, status: 'active' };
        state.licenses.push(lic);
        const code = mintCode(lic.license_id);
        const magic = `dimmy://activate?code=${code}`;
        log(`[purchase]      ${email} (${tier}) → ${magic}`);
        return ok({ magic_link: magic, code });
    },

    'GET /api/license/status': ({ query }) => {
        let licenses;
        if (query.email) licenses = state.licenses.filter(l => l.email_hash === eHash(query.email));
        else if (query.license_id) licenses = state.licenses.filter(l => l.license_id === query.license_id);
        else return err(400, 'email or license_id required');
        return ok(licenses.map(l => ({ ...l, devices: state.devices.filter(d => d.license_id === l.license_id) })));
    },

    'POST /api/devices/list': ({ body }) => {
        // Auth: caller proves identity by sending its current token.
        // Server verifies the signature, looks up the license, returns
        // the full device list (any device on the same license can list).
        if (!body.token) return err(400, 'token required');
        let claims;
        try { claims = verifyToken(body.token); } catch (e) { return err(400, `invalid token: ${e.message}`); }
        const lic = state.licenses.find(l => l.license_id === claims.lid);
        if (!lic) return err(404, 'license not found');
        const devices = state.devices.filter(d => d.license_id === lic.license_id).map(d => ({
            device_id: d.device_id,
            label: d.device_label,
            issued_at: d.issued_at,
            last_seen: d.last_seen,
            status: d.status,
            is_self: d.device_id === claims.did,
        }));
        return ok({
            license_id: lic.license_id,
            tier: lic.tier,
            max_devices: lic.max_devices,
            devices,
        });
    },

    'POST /api/devices/deactivate': ({ body }) => {
        // Either deactivate the calling device (self-revoke / sign out)
        // or another device under the same license (pruning a slot to
        // make room for a new activation).
        if (!body.token) return err(400, 'token required');
        let claims;
        try { claims = verifyToken(body.token); } catch (e) { return err(400, `invalid token: ${e.message}`); }
        const target = body.device_id || claims.did;
        const dev = state.devices.find(d => d.device_id === target && d.license_id === claims.lid);
        if (!dev) return err(404, 'device not found on this license');
        dev.status = 'deactivated';
        saveState();
        log(`[device-deactivate] lid=${claims.lid.slice(0,8)}… did=${target.slice(0,8)}… by=${claims.did.slice(0,8)}…`);
        return ok({ ok: true, device_id: target });
    },
};

// ── Server ──────────────────────────────────────────────────────────

const server = http.createServer(async (req, res) => {
    const url = new URL(req.url, 'http://localhost');
    const route = `${req.method} ${url.pathname}`;
    let body = {};
    if (req.method === 'POST') {
        try {
            body = await new Promise((resolve, reject) => {
                let buf = '';
                req.on('data', c => buf += c);
                req.on('end',   () => { try { resolve(buf ? JSON.parse(buf) : {}); } catch (e) { reject(e); } });
                req.on('error', reject);
            });
        } catch { return reply(res, 400, { error: 'invalid json body' }); }
    }
    const handler = handlers[route];
    if (!handler) return reply(res, 404, { error: 'not found' });
    try {
        const r = handler({ query: Object.fromEntries(url.searchParams), body });
        reply(res, r.status, r.body);
    } catch (e) {
        log(`[error] ${route}: ${e.message}`);
        reply(res, 500, { error: e.message });
    }
});

function reply(res, status, body) {
    res.writeHead(status, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(body));
}

server.listen(PORT, '0.0.0.0', () => {
    console.log('────────────────────────────────────────────────────');
    console.log('[dev-server] Dimmy licensing mock (Node)');
    console.log(`[dev-server] DIMMY_LICENSE_PUBKEY=${state.key.pubB64}`);
    console.log(`[dev-server] listening on ${PUBLIC_URL}`);
    console.log(`[dev-server] state file: ${STATE_FILE}`);
    console.log('────────────────────────────────────────────────────');
});
