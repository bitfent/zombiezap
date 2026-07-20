// Optional Postgres persistence (Supabase/Render-friendly). Set DATABASE_URL
// to enable; without it the server runs exactly as before — in-memory, zero
// infrastructure. Gameplay NEVER waits on the DB: matches simulate in memory
// and rows are written fire-and-forget at match end.
//
// What we persist (the minimum that real money demands):
//   matches    — one row per finished duel: who, result hash, escrow terms,
//                settlement signature. The audit trail for any dispute.
//   players    — per-wallet W/L + last seen. The seed of reputation/anti-abuse.
//   challenges — persistent shareable game offers (the durable half of the
//                lobby system). Live presence/ready/chat stay in RAM by design;
//                only the challenge itself must survive restarts so invite
//                links and scheduled matches keep working.

import postgres from "postgres";
import type { ChallengeStatus, MatchResult, WagerTerms } from "@shotante/shared";

const url = process.env.DATABASE_URL;
let sql: ReturnType<typeof postgres> | null = null;
let healthy = false;

export function dbStatus(): "off" | "ok" | "error" {
  if (!url) return "off";
  return healthy ? "ok" : "error";
}

export async function initDb(): Promise<void> {
  if (!url) return;
  try {
    // Supabase pooler wants TLS; keep the pool tiny — one game server needs
    // very few connections (writes are rare: one per finished match).
    sql = postgres(url, { ssl: "require", max: 3, onnotice: () => {} });
    await sql`
      create table if not exists matches (
        id text primary key,
        ended_at timestamptz not null default now(),
        duration_ms bigint,
        wagered boolean not null,
        match_id_hex text,
        token text,
        wager_wei text,
        p1_name text, p1_wallet text, p1_kills int,
        p2_name text, p2_wallet text, p2_kills int,
        winner_wallet text,
        reason text not null,
        result_hash text not null,
        settlement_sig text
      )`;
    await sql`
      create table if not exists players (
        wallet text primary key,
        last_name text,
        wins int not null default 0,
        losses int not null default 0,
        last_seen timestamptz not null default now()
      )`;
    // reminder_email/reminder_sent_at are written but unused until the phase-2
    // reminder sweeper ships — the schema is forward-compatible on purpose.
    await sql`
      create table if not exists challenges (
        code text primary key,
        host_key text not null,
        host_name text not null,
        host_wallet text,
        guest_name text,
        guest_wallet text,
        guest_key text,
        wagered boolean not null,
        token text,
        wager_wei text,
        is_public boolean not null,
        scheduled_at timestamptz,
        status text not null,
        created_at timestamptz not null,
        expires_at timestamptz not null,
        reminder_email text,
        reminder_sent_at timestamptz
      )`;
    healthy = true;
    console.log("db: connected, schema ready");
  } catch (err) {
    healthy = false;
    console.error("db: init failed — running without persistence:", (err as Error).message);
  }
}

// ── challenges (durable layer of the lobby system) ─────────────────────────

export interface ChallengeRecord {
  code: string;
  hostKey: string;
  hostName: string;
  hostWallet: string | null;
  guestName: string | null;
  guestWallet: string | null;
  guestKey: string | null;
  wagered: boolean;
  token: string | null;
  wagerWei: string | null;
  usdCents: number | null; // USD tier for fixed-stake wagers (stable display)
  wagerTokenKind: "usdc" | "eth" | null;
  isPublic: boolean;
  scheduledAt: number | null; // ms epoch
  status: ChallengeStatus;
  createdAt: number; // ms epoch
  expiresAt: number; // ms epoch
  reminderEmail: string | null;
}

/** Fire-and-forget; the in-memory map is the source of truth at runtime. */
export function upsertChallenge(c: ChallengeRecord): void {
  if (!sql || !healthy) return;
  void (async () => {
    try {
      await sql!`
        insert into challenges (
          code, host_key, host_name, host_wallet,
          guest_name, guest_wallet, guest_key,
          wagered, token, wager_wei, is_public,
          scheduled_at, status, created_at, expires_at, reminder_email
        ) values (
          ${c.code}, ${c.hostKey}, ${c.hostName}, ${c.hostWallet},
          ${c.guestName}, ${c.guestWallet}, ${c.guestKey},
          ${c.wagered}, ${c.token}, ${c.wagerWei}, ${c.isPublic},
          ${c.scheduledAt === null ? null : new Date(c.scheduledAt)}, ${c.status},
          ${new Date(c.createdAt)}, ${new Date(c.expiresAt)}, ${c.reminderEmail}
        ) on conflict (code) do update set
          guest_name = excluded.guest_name,
          guest_wallet = excluded.guest_wallet,
          guest_key = excluded.guest_key,
          host_wallet = excluded.host_wallet,
          status = excluded.status,
          expires_at = excluded.expires_at`;
    } catch (err) {
      console.error("db: upsertChallenge failed:", (err as Error).message);
    }
  })();
}

export function updateChallengeStatus(code: string, status: ChallengeStatus): void {
  if (!sql || !healthy) return;
  void (async () => {
    try {
      await sql!`update challenges set status = ${status} where code = ${code}`;
    } catch (err) {
      console.error("db: updateChallengeStatus failed:", (err as Error).message);
    }
  })();
}

/** Open/matched challenges that have not expired — restored into memory at boot. */
export async function loadOpenChallenges(): Promise<ChallengeRecord[]> {
  if (!sql || !healthy) return [];
  try {
    const rows = await sql`
      select * from challenges
      where status in ('open', 'matched') and expires_at > now()`;
    return rows.map((r) => ({
      code: r.code as string,
      hostKey: r.host_key as string,
      hostName: r.host_name as string,
      hostWallet: (r.host_wallet as string | null) ?? null,
      guestName: (r.guest_name as string | null) ?? null,
      guestWallet: (r.guest_wallet as string | null) ?? null,
      guestKey: (r.guest_key as string | null) ?? null,
      wagered: r.wagered as boolean,
      token: (r.token as string | null) ?? null,
      wagerWei: (r.wager_wei as string | null) ?? null,
      usdCents: (r.usd_cents as number | null) ?? null,
      wagerTokenKind: (r.wager_token_kind as "usdc" | "eth" | null) ?? null,
      isPublic: r.is_public as boolean,
      scheduledAt: r.scheduled_at ? new Date(r.scheduled_at as string).getTime() : null,
      status: r.status as ChallengeStatus,
      createdAt: new Date(r.created_at as string).getTime(),
      expiresAt: new Date(r.expires_at as string).getTime(),
      reminderEmail: (r.reminder_email as string | null) ?? null,
    }));
  } catch (err) {
    console.error("db: loadOpenChallenges failed:", (err as Error).message);
    return [];
  }
}

export interface MatchRecord {
  result: MatchResult;
  wager: WagerTerms | null;
  players: { name: string; wallet: string | null; kills: number; id: string }[];
  settlementSig: string | null;
}

/** Fire-and-forget; a DB hiccup must never affect gameplay. */
export function recordMatch(rec: MatchRecord): void {
  if (!sql || !healthy) return;
  const [p1, p2] = rec.players;
  const winner = rec.players.find((p) => p.id === rec.result.winnerId) ?? null;
  void (async () => {
    try {
      await sql!`
        insert into matches (
          id, duration_ms, wagered, match_id_hex, token, wager_wei,
          p1_name, p1_wallet, p1_kills, p2_name, p2_wallet, p2_kills,
          winner_wallet, reason, result_hash, settlement_sig
        ) values (
          ${rec.result.matchId}, ${rec.result.endedAt - rec.result.startedAt},
          ${rec.wager !== null}, ${rec.wager?.matchIdHex ?? null},
          ${rec.wager?.token ?? null}, ${rec.wager?.wagerWei ?? null},
          ${p1.name}, ${p1.wallet}, ${p1.kills},
          ${p2.name}, ${p2.wallet}, ${p2.kills},
          ${winner?.wallet ?? null}, ${rec.result.reason}, ${rec.result.resultHash},
          ${rec.settlementSig}
        ) on conflict (id) do nothing`;
      for (const p of rec.players) {
        if (!p.wallet) continue; // anonymous free players have no stable identity
        const won = p.id === rec.result.winnerId;
        await sql!`
          insert into players (wallet, last_name, wins, losses)
          values (${p.wallet}, ${p.name}, ${won ? 1 : 0}, ${won ? 0 : 1})
          on conflict (wallet) do update set
            last_name = excluded.last_name,
            wins = players.wins + ${won ? 1 : 0},
            losses = players.losses + ${won ? 0 : 1},
            last_seen = now()`;
      }
    } catch (err) {
      console.error("db: recordMatch failed:", (err as Error).message);
    }
  })();
}
