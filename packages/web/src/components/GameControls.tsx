import { Button } from "@/components/ui/button";
import { Redo, Undo } from "lucide-react";
import type React from "react";

interface GameControlsProps {
    canUndo: boolean;
    canRedo: boolean;
    onUndo: () => void;
    onRedo: () => void;
    isOnlineGame?: boolean;
}

export const GameControls: React.FC<GameControlsProps> = ({
    canUndo,
    canRedo,
    onUndo,
    onRedo,
    isOnlineGame = false,
}) => {
    return (
        <div className="flex items-center justify-center gap-2">
            <Button
                variant="outline"
                size="sm"
                onClick={onUndo}
                disabled={!canUndo}
                title={!canUndo && isOnlineGame ? "通信対局中は使用できません" : "1手戻す (Ctrl+Z)"}
            >
                <Undo className="h-4 w-4 mr-1" />
                戻す
            </Button>
            <Button
                variant="outline"
                size="sm"
                onClick={onRedo}
                disabled={!canRedo}
                title={
                    !canRedo && isOnlineGame
                        ? "通信対局中は使用できません"
                        : "1手進む (Ctrl+Shift+Z)"
                }
            >
                <Redo className="h-4 w-4 mr-1" />
                進む
            </Button>
        </div>
    );
};
