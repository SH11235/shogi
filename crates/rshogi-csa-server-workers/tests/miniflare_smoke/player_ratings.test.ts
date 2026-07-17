import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { Miniflare } from "miniflare";
import { createMiniflare, makeTempPersistRoot } from "./harness.ts";

describe("miniflare smoke: materialized player ratings API", () => {
  let mf: Miniflare;
  let cleanupPersist: () => Promise<void>;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanupPersist = persist.cleanup;
    mf = await createMiniflare({
      persistRoot: persist.path,
      allowViewerApi: true,
      adminApiToken: "ratings-admin-token",
    });
    const db = await mf.getD1Database("GAMES_SEARCH_DB");
    const insert = db.prepare(
      `INSERT INTO games_search_index
       (game_id, sente_name, gote_name, black_player_id, white_player_id,
        started_at_ms, ended_at_ms, result_kind, source, moves_count,
        wire_result_kind, end_reason, clock_json)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
    );
    const clock = JSON.stringify({ kind: "fischer", total_sec: 300, increment_sec: 5 });
    await db.batch([
      insert.bind("g1", "Alice", "Bob", "p_alice_old_59", "p_bob", 10, 20, "resignation", "kifu", 40, "WIN_BLACK", "RESIGN", clock),
      insert.bind("g2", "Bob", "Alice", "p_bob", "p_alice", 30, 40, "resignation", "kifu", 42, "WIN_WHITE", "RESIGN", clock),
      insert.bind("g3", "Alice", "Carol", "p_alice", "p_carol", 50, 60, "draw", "kifu", 60, "DRAW", "SENNICHITE", clock),
    ]);
    const aliasInsert = db.prepare(
      "INSERT INTO player_id_aliases (alias_id, canonical_id) VALUES (?, ?)",
    );
    await db.batch(
      Array.from({ length: 60 }, (_, index) =>
        aliasInsert.bind(`p_alice_old_${index}`, "p_alice"),
      ),
    );
  });

  afterEach(async () => {
    if (mf) await mf.dispose();
    if (cleanupPersist) await cleanupPersist();
  });

  it("authenticates warmup then serves list and paginated detail from active generation", async () => {
    const denied = await warmup("wrong-token");
    expect(denied.status).toBe(403);

    const warmed = await warmup("ratings-admin-token");
    expect(warmed.status).toBe(200);
    expect(await warmed.json()).toMatchObject({
      processed_games: 3,
      ready: true,
      rebuild_in_progress: false,
      lease_acquired: true,
    });

    const list = await viewerFetch("/api/v1/players?page=2&pageSize=1");
    expect(list.status).toBe(200);
    const listBody = (await list.json()) as {
      players: Array<{ player_id: string; games: number; rating: number }>;
      page: number;
      page_size: number;
      total_count: number;
      total_games: number;
      leader: { player_id: string; games: number } | null;
    };
    expect(listBody).toMatchObject({
      page: 2,
      page_size: 1,
      total_count: 3,
      total_games: 3,
      leader: { player_id: "p_alice", games: 3 },
    });
    expect(listBody.players).toHaveLength(1);

    // Resolve through one of 60 historical aliases. The API query uses four
    // fixed binds, so the family size cannot hit D1's parameter ceiling.
    const firstPage = await viewerFetch("/api/v1/players/p_alice_old_0?page=1&pageSize=1");
    expect(firstPage.status).toBe(200);
    const first = (await firstPage.json()) as {
      player: { player_id: string };
      games: Array<{ game_id: string }>;
      page: number;
      page_size: number;
      total_count: number;
    };
    expect(first).toMatchObject({
      player: { player_id: "p_alice" },
      page: 1,
      page_size: 1,
      total_count: 3,
    });
    expect(first.games).toHaveLength(1);
    expect(first.games[0]?.game_id).toBe("g3");

    const secondPage = await viewerFetch("/api/v1/players/p_alice?page=2&pageSize=1");
    expect(secondPage.status).toBe(200);
    const second = (await secondPage.json()) as { games: Array<{ game_id: string }> };
    expect(second.games[0]?.game_id).toBe("g2");
  });

  it("discards a page loaded before a same-game data revision change", async () => {
    const db = await mf.getD1Database("GAMES_SEARCH_DB");
    await db
      .prepare(
        "UPDATE player_rating_state SET rebuild_required = 0, building_generation = 1 WHERE singleton = 1",
      )
      .run();
    const loaded = await db
      .prepare("SELECT data_revision FROM player_rating_state WHERE singleton = 1")
      .first<{ data_revision: number }>();
    expect(loaded?.data_revision).toBe(0);

    // This is the invariant used by every real changed game UPSERT: revision
    // advancement and the row mutation commit atomically.
    await db.batch([
      db.prepare(
        "UPDATE player_rating_state SET data_revision = data_revision + 1 WHERE singleton = 1",
      ),
      db.prepare("UPDATE games_search_index SET moves_count = moves_count + 1 WHERE game_id = ?").bind("g1"),
    ]);

    const expectedRevision = loaded!.data_revision;
    await db.batch([
      db
        .prepare(
          `INSERT INTO player_rating_generations
           (generation, player_id, display_name, rating, wins, losses, draws, games,
            last_played_at_ms, legacy)
           SELECT 1, 'must_not_commit', 'stale', 1500, 0, 0, 0, 0, 20, 0
           WHERE (SELECT data_revision FROM player_rating_state WHERE singleton = 1) = ?`,
        )
        .bind(expectedRevision),
      db
        .prepare(
          "UPDATE player_rating_state SET cursor_ended_at_ms = 20, cursor_game_id = 'g1' WHERE singleton = 1 AND data_revision = ?",
        )
        .bind(expectedRevision),
      db
        .prepare(
          "UPDATE player_rating_state SET rebuild_required = 1 WHERE singleton = 1 AND data_revision <> ?",
        )
        .bind(expectedRevision),
    ]);

    const stale = await db
      .prepare("SELECT player_id FROM player_rating_generations WHERE player_id = 'must_not_commit'")
      .first();
    const state = await db
      .prepare(
        "SELECT cursor_ended_at_ms, cursor_game_id, rebuild_required, data_revision FROM player_rating_state WHERE singleton = 1",
      )
      .first<{
        cursor_ended_at_ms: number;
        cursor_game_id: string;
        rebuild_required: number;
        data_revision: number;
      }>();
    expect(stale).toBeNull();
    expect(state).toEqual({
      cursor_ended_at_ms: -1,
      cursor_game_id: "",
      rebuild_required: 1,
      data_revision: 1,
    });
  });

  async function warmup(token: string): Promise<Response> {
    return mf.dispatchFetch("https://example.com/api/v1/admin/player-ratings/warmup", {
      method: "POST",
      headers: { Authorization: `Bearer ${token}` },
    });
  }

  async function viewerFetch(path: string): Promise<Response> {
    return mf.dispatchFetch(`https://example.com${path}`, {
      headers: { Origin: "https://example.com", "X-Client": "ratings-smoke/1" },
    });
  }
});
