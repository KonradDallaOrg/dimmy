// Minimal in-memory D1 mock — implements just the subset of D1Database
// the production code uses (prepare → bind → first / all / run).
//
// We reimplement SQL only for the queries we actually run in handlers,
// pattern-matched by SQL substring + arity. This is faster + simpler
// than embedding sqlite-wasm and easier to assert against in tests.

export interface MockState {
  licenses: Map<string, Record<string, unknown>>;
  devices: Map<string, Record<string, unknown>>;
  activation_codes: Map<string, Record<string, unknown>>;
  stripe_events: Map<string, Record<string, unknown>>;
  audit_log: Record<string, unknown>[];
}

export function emptyState(): MockState {
  return {
    licenses: new Map(),
    devices: new Map(),
    activation_codes: new Map(),
    stripe_events: new Map(),
    audit_log: [],
  };
}

interface PreparedStatement {
  bind: (...args: unknown[]) => PreparedStatement;
  first: <T = unknown>() => Promise<T | null>;
  all: <T = unknown>() => Promise<{ results: T[] }>;
  run: () => Promise<{ meta: { changes: number; last_row_id?: number } }>;
}

export function makeMockDB(state: MockState): {
  prepare: (sql: string) => PreparedStatement;
} {
  return {
    prepare(sql: string): PreparedStatement {
      const trimmed = sql.replace(/\s+/g, " ").trim();
      let bound: unknown[] = [];
      const stmt: PreparedStatement = {
        bind(...args) {
          bound = args;
          return stmt;
        },
        async first<T>() {
          return execFirst<T>(state, trimmed, bound);
        },
        async all<T>() {
          return execAll<T>(state, trimmed, bound);
        },
        async run() {
          return execRun(state, trimmed, bound);
        },
      };
      return stmt;
    },
  };
}

// ── query dispatch ─────────────────────────────────────────────────

function execFirst<T>(state: MockState, sql: string, bound: unknown[]): T | null {
  if (sql.includes("FROM licenses WHERE email_hash = ?1 AND status = 'active'")) {
    const eh = bound[0] as string;
    const rows = [...state.licenses.values()]
      .filter((l) => l.email_hash === eh && l.status === "active")
      .sort((a, b) => (b.issued_at as number) - (a.issued_at as number));
    return (rows[0] as T) ?? null;
  }
  if (sql.includes("FROM licenses WHERE license_id = ?1")) {
    return (state.licenses.get(bound[0] as string) as T) ?? null;
  }
  if (sql.includes("FROM licenses WHERE stripe_session_id = ?1")) {
    const sid = bound[0] as string;
    return ((
      [...state.licenses.values()].find((l) => l.stripe_session_id === sid) as T
    ) ?? null);
  }
  if (sql.includes("FROM licenses WHERE stripe_subscription_id = ?1")) {
    const sub = bound[0] as string;
    return ((
      [...state.licenses.values()].find(
        (l) => l.stripe_subscription_id === sub
      ) as T
    ) ?? null);
  }
  if (
    sql.includes("FROM licenses") &&
    sql.includes("WHERE stripe_customer_id = ?1") &&
    sql.includes("AND status = 'active'")
  ) {
    const cust = bound[0] as string;
    const matches = [...state.licenses.values()]
      .filter((l) => l.stripe_customer_id === cust && l.status === "active")
      .sort((a, b) => b.issued_at - a.issued_at);
    return ((matches[0] as T) ?? null);
  }
  if (sql.includes("SELECT COUNT(*) as n FROM devices")) {
    const lid = bound[0] as string;
    const n = [...state.devices.values()].filter(
      (d) => d.license_id === lid && d.status === "active"
    ).length;
    return { n } as unknown as T;
  }
  if (sql.includes("FROM activation_codes WHERE code = ?1")) {
    return (state.activation_codes.get(bound[0] as string) as T) ?? null;
  }
  if (sql.includes("SELECT status FROM devices WHERE device_id = ?1")) {
    const did = bound[0] as string;
    const lid = bound[1] as string;
    const d = [...state.devices.values()].find(
      (x) => x.device_id === did && x.license_id === lid
    );
    return (d ? ({ status: d.status } as unknown as T) : null);
  }
  throw new Error(`unhandled first() SQL: ${sql.slice(0, 80)}…`);
}

function execAll<T>(
  state: MockState,
  sql: string,
  bound: unknown[]
): { results: T[] } {
  if (sql.includes("FROM devices WHERE license_id = ?1 AND status = 'active'")) {
    const lid = bound[0] as string;
    const results = [...state.devices.values()].filter(
      (d) => d.license_id === lid && d.status === "active"
    );
    return { results: results as T[] };
  }
  if (sql.includes("FROM devices WHERE license_id = ?1 ORDER BY issued_at")) {
    const lid = bound[0] as string;
    return {
      results: [...state.devices.values()].filter(
        (d) => d.license_id === lid
      ) as T[],
    };
  }
  throw new Error(`unhandled all() SQL: ${sql.slice(0, 80)}…`);
}

function execRun(
  state: MockState,
  sql: string,
  bound: unknown[]
): { meta: { changes: number } } {
  if (sql.startsWith("INSERT INTO licenses")) {
    const row = {
      license_id: bound[0],
      email_hash: bound[1],
      tier: bound[2],
      issued_at: bound[3],
      valid_until: bound[4],
      max_devices: 5,
      status: "active",
      stripe_session_id: bound[5] ?? null,
      stripe_customer_id: bound[6] ?? null,
      stripe_subscription_id: bound[7] ?? null,
      current_period_end: bound[8] ?? null,
      cancel_at_period_end: 0,
    };
    state.licenses.set(row.license_id as string, row);
    return { meta: { changes: 1 } };
  }
  if (sql.includes("UPDATE licenses SET status = ?1")) {
    const lic = state.licenses.get(bound[1] as string);
    if (!lic) return { meta: { changes: 0 } };
    lic.status = bound[0];
    return { meta: { changes: 1 } };
  }
  if (sql.includes("UPDATE licenses SET")) {
    // updateLicenseFromSubscription — COALESCE patch.
    const subId = bound[4] as string;
    const lic = [...state.licenses.values()].find(
      (l) => l.stripe_subscription_id === subId
    );
    if (!lic) return { meta: { changes: 0 } };
    if (bound[0] !== null) lic.valid_until = bound[0];
    if (bound[1] !== null) lic.current_period_end = bound[1];
    if (bound[2] !== null) lic.cancel_at_period_end = bound[2];
    if (bound[3] !== null) lic.status = bound[3];
    return { meta: { changes: 1 } };
  }
  if (sql.startsWith("INSERT INTO devices")) {
    const row = {
      device_id: bound[0],
      license_id: bound[1],
      device_label: bound[2],
      issued_at: bound[3],
      last_seen: bound[3],
      status: "active",
    };
    state.devices.set(row.device_id as string, row);
    return { meta: { changes: 1 } };
  }
  if (sql.includes("UPDATE devices SET status = 'revoked'")) {
    const did = bound[0] as string;
    const lid = bound[1] as string;
    const d = [...state.devices.values()].find(
      (x) => x.device_id === did && x.license_id === lid && x.status === "active"
    );
    if (!d) return { meta: { changes: 0 } };
    d.status = "revoked";
    return { meta: { changes: 1 } };
  }
  if (sql.includes("UPDATE devices SET last_seen = ?1")) {
    const d = [...state.devices.values()].find(
      (x) => x.device_id === bound[1]
    );
    if (!d) return { meta: { changes: 0 } };
    d.last_seen = bound[0];
    return { meta: { changes: 1 } };
  }
  if (sql.startsWith("INSERT INTO activation_codes")) {
    const row = {
      code: bound[0],
      license_id: bound[1],
      created_at: bound[2],
      expires_at: bound[3],
      consumed_at: null,
    };
    state.activation_codes.set(row.code as string, row);
    return { meta: { changes: 1 } };
  }
  if (sql.includes("UPDATE activation_codes SET consumed_at = ?1")) {
    const c = state.activation_codes.get(bound[1] as string);
    if (!c || c.consumed_at !== null) return { meta: { changes: 0 } };
    c.consumed_at = bound[0];
    return { meta: { changes: 1 } };
  }
  if (sql.startsWith("INSERT OR IGNORE INTO stripe_events")) {
    const eid = bound[0] as string;
    if (state.stripe_events.has(eid)) return { meta: { changes: 0 } };
    state.stripe_events.set(eid, {
      event_id: eid,
      received_at: bound[1],
      type: bound[2],
    });
    return { meta: { changes: 1 } };
  }
  if (sql.startsWith("INSERT INTO audit_log")) {
    state.audit_log.push({
      timestamp: bound[0],
      event_type: bound[1],
      email_hash: bound[2],
      license_id: bound[3],
      details: bound[4],
    });
    return { meta: { changes: 1 } };
  }
  throw new Error(`unhandled run() SQL: ${sql.slice(0, 80)}…`);
}
