import { initialBoard } from "@/domain/initialBoard";
import type { Board } from "@/domain/model/board";
import type { Move } from "@/domain/model/move";
import type { Player } from "@/domain/model/piece";
import type { Column, Row, SquareKey } from "@/domain/model/square";
import { type Hands, applyMove, createEmptyHands } from "./moveService";

/**
 * Convert board, hands and turn into a stable string key.
 */
const stateKey = (board: Board, hands: Hands, turn: Player): string => {
    const cells: string[] = [];
    for (let r = 1 as Row; r <= 9; r++) {
        for (let c = 1 as Column; c <= 9; c++) {
            const sq = `${r}${c}` as SquareKey;
            const p = board[sq];
            if (!p) {
                cells.push(".");
            } else {
                cells.push(`${p.owner[0]}${p.kind}${p.promoted ? "+" : "-"}`);
            }
        }
    }
    return `${cells.join("")}|${JSON.stringify(hands)}|${turn}`;
};

/**
 * Determine if the same board position with identical hands and turn
 * appears four times within the given move history.
 */
export const isRepetition = (history: Move[]): boolean => {
    let board = initialBoard;
    let hands = createEmptyHands();
    let turn: Player = "black";

    const counts = new Map<string, number>();
    const record = () => {
        const key = stateKey(board, hands, turn);
        const v = (counts.get(key) ?? 0) + 1;
        counts.set(key, v);
        return v;
    };

    if (record() >= 4) return true;
    for (const mv of history) {
        const res = applyMove(board, hands, turn, mv);
        board = res.board;
        hands = res.hands;
        turn = res.nextTurn;
        if (record() >= 4) return true;
    }
    return false;
};
