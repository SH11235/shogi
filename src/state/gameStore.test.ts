import { initialBoard } from "@/domain/initialBoard";
import type { Board } from "@/domain/model/board";
import type { Move } from "@/domain/model/move";
import type { Piece } from "@/domain/model/piece";
import { createEmptyHands } from "@/domain/service/moveService";
import { describe, expect, it } from "vitest";
import { useGameStore } from "./gameStore";

// 簡易盤面設定ユーティリティ
const place = (board: Board, key: string, piece: Piece): Board => ({
    ...board,
    [key]: piece,
});

const cycleMoves = (): Move[] => [
    {
        type: "move",
        from: { row: 8, column: 8 },
        to: { row: 8, column: 7 },
        piece: { kind: "飛", promoted: false, owner: "black" },
        promote: false,
        captured: null,
    },
    {
        type: "move",
        from: { row: 2, column: 8 },
        to: { row: 2, column: 7 },
        piece: { kind: "飛", promoted: false, owner: "white" },
        promote: false,
        captured: null,
    },
    {
        type: "move",
        from: { row: 8, column: 7 },
        to: { row: 8, column: 8 },
        piece: { kind: "飛", promoted: false, owner: "black" },
        promote: false,
        captured: null,
    },
    {
        type: "move",
        from: { row: 2, column: 7 },
        to: { row: 2, column: 8 },
        piece: { kind: "飛", promoted: false, owner: "white" },
        promote: false,
        captured: null,
    },
];

describe("useGameStore makeMove", () => {
    it("updates result when move results in checkmate", () => {
        let board: Board = {} as Board;
        board = place(board, "55", { kind: "王", promoted: false, owner: "black" });
        board = place(board, "45", { kind: "飛", promoted: false, owner: "white" });
        board = place(board, "54", { kind: "銀", promoted: false, owner: "white" });
        board = place(board, "56", { kind: "銀", promoted: false, owner: "white" });
        board = place(board, "65", { kind: "桂", promoted: false, owner: "white" });
        board = place(board, "44", { kind: "金", promoted: false, owner: "white" });
        board = place(board, "46", { kind: "金", promoted: false, owner: "white" });
        board = place(board, "64", { kind: "金", promoted: false, owner: "white" });
        board = place(board, "66", { kind: "金", promoted: false, owner: "white" });

        const hands = createEmptyHands();
        hands.white.歩 = 1;

        useGameStore.setState({
            board,
            hands,
            history: [],
            cursor: -1,
            turn: "white",
            result: null,
        });

        const move: Move = {
            type: "drop",
            to: { row: 1, column: 1 },
            piece: { kind: "歩", promoted: false, owner: "white" },
        };

        useGameStore.getState().makeMove(move);

        expect(useGameStore.getState().result).toEqual({
            winner: "white",
            reason: "checkmate",
        });
    });

    it("sets draw result when fourfold repetition occurs", () => {
        useGameStore.setState({
            board: initialBoard,
            hands: createEmptyHands(),
            history: [],
            cursor: -1,
            turn: "black",
            result: null,
        });

        const history: Move[] = [...cycleMoves(), ...cycleMoves(), ...cycleMoves()];
        for (const mv of history) {
            useGameStore.getState().makeMove(mv);
        }

        expect(useGameStore.getState().result).toEqual({
            winner: null,
            reason: "repetition",
        });
    });
});
