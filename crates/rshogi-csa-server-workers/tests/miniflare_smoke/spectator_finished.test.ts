import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { Miniflare, WebSocket } from "miniflare";
import {
  CsaClient,
  DEFAULT_TEST_CF_CONNECTING_IP,
  createMiniflare,
  makeTempPersistRoot,
} from "./harness.ts";
import { readLineFromWebSocket } from "./ws_test_helpers";

describe("miniflare smoke: 終局済み spectate", () => {
  let mf: Miniflare;
  let cleanupPersist: () => Promise<void>;

  beforeEach(async () => {
    const persist = await makeTempPersistRoot();
    cleanupPersist = persist.cleanup;
    mf = await createMiniflare({
      persistRoot: persist.path,
      allowViewerApi: true,
      totalTimeSec: 60,
      byoyomiSec: 1,
    });
  });

  afterEach(async () => {
    await mf.dispose();
    await cleanupPersist();
  });

  it("終局済み room への spectate は MONITOR2ON を待たずに snapshot と結果コードを push して正常 close する", async () => {
    const { gameId } = await finishOneGame(mf, "spectate-finished-room-1");

    const res = await dispatchSpectator(mf, gameId);
    expect(res.status).toBe(101);
    expect(res.webSocket).toBeTruthy();
    const ws = res.webSocket!;
    const buf = readLineFromWebSocket(ws);
    const closeInfo = waitForClose(ws);
    ws.accept();

    const lines = await readUntil(buf, "##[MONITOR2] END");
    expect(lines[0]).toBe(`##[MONITOR2] BEGIN ${gameId}`);
    expect(lines).toContain("BEGIN Game_Summary");
    expect(lines).toContain(`Game_ID:${gameId}`);
    expect(lines).toContain("#RESIGN");
    expect(lines.at(-1)).toBe("##[MONITOR2] END");

    const closed = await closeInfo;
    expect(closed.code).toBe(1000);
    expect(closed.reason).toBe("spectate finished");
  });

  it("spectator close 後は観戦上限カウントから外れ、50 枠を再利用できる", async () => {
    const { gameId } = await startOneGame(mf, "spectate-active-room-1");

    const first = await connectSpectator(mf, gameId);
    await closeWebSocket(first);

    const opened: WebSocket[] = [];
    try {
      for (let i = 0; i < 50; i += 1) {
        opened.push(await connectSpectator(mf, gameId, `127.0.1.${i + 1}`));
      }
      const full = await dispatchSpectator(mf, gameId, "127.0.2.1");
      expect(full.status).toBe(101);
      expect(full.webSocket).toBeTruthy();
      const fullWs = full.webSocket!;
      const fullClose = waitForClose(fullWs);
      fullWs.accept();
      await expect(fullClose).resolves.toEqual({ code: 1013, reason: "room full" });

      const reusable = opened.pop();
      expect(reusable).toBeTruthy();
      await closeWebSocket(reusable!);

      const replacement = await dispatchSpectator(mf, gameId, "127.0.2.2");
      expect(replacement.status).toBe(101);
      expect(replacement.webSocket).toBeTruthy();
      const replacementWs = replacement.webSocket!;
      const noClose = expectNoClose(replacementWs);
      replacementWs.accept();
      await noClose;
      opened.push(replacementWs);
    } finally {
      for (const ws of opened) {
        ws.close();
      }
    }
  });
});

interface StartedGame {
  gameId: string;
  black: CsaClient;
  white: CsaClient;
}

async function startOneGame(mf: Miniflare, roomId: string): Promise<StartedGame> {
  const gameName = `${roomId}-game`;
  const black = await CsaClient.connect(mf, roomId);
  const blackName = `alice+${gameName}+black`;
  black.send(`LOGIN ${blackName} pw`);
  expect(await black.recvLine()).toBe(`LOGIN:${blackName} OK`);

  const white = await CsaClient.connect(mf, roomId);
  const whiteName = `bob+${gameName}+white`;
  white.send(`LOGIN ${whiteName} pw`);
  expect(await white.recvLine()).toBe(`LOGIN:${whiteName} OK`);

  await black.drainGameSummary();
  await white.drainGameSummary();

  black.send("AGREE");
  white.send("AGREE");
  const startBlack = await black.recvLine();
  const startWhite = await white.recvLine();
  expect(startBlack).toBe(startWhite);
  expect(startBlack.startsWith("START:")).toBe(true);

  return { gameId: startBlack.slice("START:".length), black, white };
}

async function finishOneGame(
  mf: Miniflare,
  roomId: string,
): Promise<{ gameId: string }> {
  const game = await startOneGame(mf, roomId);

  game.black.send("+7776FU");
  await game.black.recvUntil((l) => l.startsWith("+7776FU"));
  await game.white.recvUntil((l) => l.startsWith("+7776FU"));

  game.white.send("-3334FU");
  await game.black.recvUntil((l) => l.startsWith("-3334FU"));
  await game.white.recvUntil((l) => l.startsWith("-3334FU"));

  game.black.send("%TORYO");
  const blackEnd = await game.black.recvUntil((l) => l === "#LOSE");
  expect(blackEnd).toContain("#RESIGN");

  await game.black.close();
  await game.white.close();
  return { gameId: game.gameId };
}

async function dispatchSpectator(
  mf: Miniflare,
  gameId: string,
  cfConnectingIp = DEFAULT_TEST_CF_CONNECTING_IP,
): Promise<Response> {
  return await mf.dispatchFetch(`https://example.com/ws/${encodeURIComponent(gameId)}/spectate`, {
    headers: {
      Upgrade: "websocket",
      Origin: "https://example.com",
      "CF-Connecting-IP": cfConnectingIp,
    },
  });
}

async function connectSpectator(
  mf: Miniflare,
  gameId: string,
  cfConnectingIp = DEFAULT_TEST_CF_CONNECTING_IP,
): Promise<WebSocket> {
  const res = await dispatchSpectator(mf, gameId, cfConnectingIp);
  if (res.status !== 101 || !res.webSocket) {
    throw new Error(`expected 101 with webSocket, got ${res.status}: ${await res.text()}`);
  }
  res.webSocket.accept();
  return res.webSocket;
}

async function readUntil(
  buf: ReturnType<typeof readLineFromWebSocket>,
  terminal: string,
): Promise<string[]> {
  const lines: string[] = [];
  while (true) {
    const line = await buf.takeLine(5000);
    lines.push(line);
    if (line === terminal) return lines;
  }
}

async function closeWebSocket(ws: WebSocket): Promise<void> {
  if (ws.readyState === 2 || ws.readyState === 3) return;
  await new Promise<void>((resolve) => {
    const timer = setTimeout(resolve, 1000);
    ws.addEventListener(
      "close",
      () => {
        clearTimeout(timer);
        resolve();
      },
      { once: true },
    );
    ws.close();
  });
  await new Promise((resolve) => setTimeout(resolve, 25));
}

async function waitForClose(ws: WebSocket): Promise<{ code: number; reason: string }> {
  if (ws.readyState === 3) return { code: 1000, reason: "" };
  return await new Promise((resolve) => {
    ws.addEventListener(
      "close",
      (ev) => {
        resolve({ code: ev.code, reason: ev.reason });
      },
      { once: true },
    );
  });
}

async function expectNoClose(ws: WebSocket): Promise<void> {
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(resolve, 100);
    ws.addEventListener(
      "close",
      (ev) => {
        clearTimeout(timer);
        reject(new Error(`unexpected close: ${ev.code} ${ev.reason}`));
      },
      { once: true },
    );
  });
}
