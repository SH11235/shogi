import type { Move } from "@/domain/model/move";
import type { Piece } from "@/domain/model/piece";
import type { Column, Row, Square } from "@/domain/model/square";
import { describe, expect, it } from "vitest";
import { isRepetition } from "./repetition";

const sq = (row: Row, col: Column): Square => ({ row, column: col });
const makePiece = (kind: Piece["kind"], owner: Piece["owner"], promoted = false): Piece => ({
    kind,
    owner,
    promoted,
});

const cycleMoves = (): Move[] => [
    {
        type: "move",
        from: sq(8, 8),
        to: sq(8, 7),
        piece: makePiece("飛", "black"),
        promote: false,
        captured: null,
    },
    {
        type: "move",
        from: sq(2, 8),
        to: sq(2, 7),
        piece: makePiece("飛", "white"),
        promote: false,
        captured: null,
    },
    {
        type: "move",
        from: sq(8, 7),
        to: sq(8, 8),
        piece: makePiece("飛", "black"),
        promote: false,
        captured: null,
    },
    {
        type: "move",
        from: sq(2, 7),
        to: sq(2, 8),
        piece: makePiece("飛", "white"),
        promote: false,
        captured: null,
    },
];

describe("isRepetition", () => {
    it("returns true when a board state appears four times", () => {
        const history: Move[] = [...cycleMoves(), ...cycleMoves(), ...cycleMoves()];
        expect(isRepetition(history)).toBe(true);
    });

    it("returns false when repetition count is below four", () => {
        const history: Move[] = [...cycleMoves(), ...cycleMoves()];
        expect(isRepetition(history)).toBe(false);
    });
});
