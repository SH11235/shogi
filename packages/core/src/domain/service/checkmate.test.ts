import { describe, expect, it } from "vitest";
import type { Board } from "../model/board";
import type { Piece } from "../model/piece";
import type { Square } from "../model/square";
import { isCheckmate } from "./checkmate";
import { initialHands } from "./moveService";

// 駒配置ユーティリティ
const place = (board: Board, square: Square, piece: Piece): Board => ({
    ...board,
    [`${square.row}${square.column}`]: piece,
});

describe("Checkmate detection tests", () => {
    it("王手されていない場合は詰みでない", () => {
        const board: Board = {
            "55": {
                type: "king",
                promoted: false,
                owner: "black",
            },
            "11": {
                type: "pawn",
                promoted: false,
                owner: "white",
            },
        } as Board;

        const hands = initialHands();
        const result = isCheckmate(board, hands, "black");
        expect(result).toBe(false);
    });

    it("王の逃げ道がある場合は詰みでない", () => {
        let board: Board = {} as Board;
        board = place(
            board,
            { row: 5, column: 5 },
            {
                type: "king",
                promoted: false,
                owner: "black",
            },
        );
        board = place(
            board,
            { row: 6, column: 5 },
            {
                type: "rook",
                promoted: false,
                owner: "white",
            },
        );

        const hands = initialHands();
        const result = isCheckmate(board, hands, "black");
        expect(result).toBe(false); // 王は左や右に逃げられる
    });

    it("逃げ道がなく、詰みである場合", () => {
        let board: Board = {} as Board;
        board = place(
            board,
            { row: 5, column: 5 },
            { type: "king", promoted: false, owner: "black" },
        );
        board = place(
            board,
            { row: 4, column: 5 },
            { type: "rook", promoted: false, owner: "white" },
        ); // 上
        board = place(
            board,
            { row: 5, column: 4 },
            { type: "silver", promoted: false, owner: "white" },
        ); // 左
        board = place(
            board,
            { row: 5, column: 6 },
            { type: "silver", promoted: false, owner: "white" },
        ); // 右
        board = place(
            board,
            { row: 6, column: 5 },
            { type: "knight", promoted: false, owner: "white" },
        ); // 下
        board = place(
            board,
            { row: 4, column: 4 },
            { type: "gold", promoted: false, owner: "white" },
        ); // 左上
        board = place(
            board,
            { row: 4, column: 6 },
            { type: "gold", promoted: false, owner: "white" },
        ); // 右上
        board = place(
            board,
            { row: 6, column: 4 },
            { type: "gold", promoted: false, owner: "white" },
        ); // 左下
        board = place(
            board,
            { row: 6, column: 6 },
            { type: "gold", promoted: false, owner: "white" },
        ); // 右下

        const hands = initialHands();
        const result = isCheckmate(board, hands, "black");
        expect(result).toBe(true); // 王がどこにも動けない
    });

    it("玉を使用する後手の王が無攻撃なら詰みでない", () => {
        const board: Board = {
            "55": {
                type: "gyoku",
                promoted: false,
                owner: "white",
            },
        } as Board;

        const hands = initialHands();
        const result = isCheckmate(board, hands, "white");
        expect(result).toBe(false);
    });

    it("駒打ちで王手を防いで詰みを回避できる", () => {
        let board: Board = {} as Board;
        board = place(
            board,
            { row: 5, column: 5 },
            { type: "king", promoted: false, owner: "black" },
        );
        board = place(
            board,
            { row: 1, column: 5 },
            { type: "rook", promoted: false, owner: "white" },
        );
        board = place(
            board,
            { row: 5, column: 4 },
            { type: "silver", promoted: false, owner: "white" },
        );
        board = place(
            board,
            { row: 5, column: 6 },
            { type: "silver", promoted: false, owner: "white" },
        );
        board = place(
            board,
            { row: 6, column: 5 },
            { type: "knight", promoted: false, owner: "white" },
        );
        board = place(
            board,
            { row: 6, column: 4 },
            { type: "gold", promoted: false, owner: "white" },
        );
        board = place(
            board,
            { row: 6, column: 6 },
            { type: "gold", promoted: false, owner: "white" },
        );
        board = place(
            board,
            { row: 4, column: 4 },
            { type: "pawn", promoted: false, owner: "black" },
        );
        board = place(
            board,
            { row: 4, column: 6 },
            { type: "pawn", promoted: false, owner: "black" },
        );

        const noHand = initialHands();
        expect(isCheckmate(board, noHand, "black")).toBe(true);

        const hands = initialHands();
        hands.black.金 = 1;
        expect(isCheckmate(board, hands, "black")).toBe(false);
    });
});
