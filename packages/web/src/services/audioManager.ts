import type {
    AudioManager,
    AudioPlayerState,
    PlayOptions,
    SoundConfig,
    SoundMap,
    SoundType,
} from "@/types/audio";
import { volumeLevelToFloat } from "@/types/audio";
import type { VolumeLevel } from "@/types/settings";
import { createAudioBlobURL, generateSoundForType, revokeBlobURL } from "./audioGenerator";

/**
 * 音声ファイルの設定
 */
const SOUND_FILES: SoundMap = {
    piece: {
        path: "/sounds/piece.mp3",
        defaultVolume: 75,
        preload: true,
    },
    check: {
        path: "/sounds/check.mp3",
        defaultVolume: 75,
        preload: true,
    },
    gameEnd: {
        path: "/sounds/game-end.mp3",
        defaultVolume: 100,
        preload: false,
    },
};

/**
 * AudioPoolインターフェース
 */
interface AudioPool {
    getAudio: (type: SoundType, config: SoundConfig) => Promise<HTMLAudioElement>;
    stopAll: () => void;
    preload: (type: SoundType, config: SoundConfig) => Promise<void>;
    cleanup: () => void;
}

/**
 * HTMLAudioElementのプールを作成する関数
 */
function createAudioPool(): AudioPool {
    const pool = new Map<SoundType, HTMLAudioElement[]>();
    const generatedUrls = new Map<SoundType, string>();
    const maxPoolSize = 3; // 同時再生可能数

    const getAudioUrl = async (type: SoundType): Promise<string> => {
        // 既に生成済みのURLがあれば再利用
        const existingUrl = generatedUrls.get(type);
        if (existingUrl) {
            return existingUrl;
        }

        // 実際の音声ファイルの確認はスキップし、直接生成された音声を使用
        // （開発環境では音声ファイルがないため）

        try {
            const blob = await generateSoundForType(type);
            const url = createAudioBlobURL(blob);
            generatedUrls.set(type, url);
            return url;
        } catch (error) {
            console.error(`AudioPool.getAudioUrl: Failed to generate sound for ${type}:`, error);
            throw error;
        }
    };

    const getAudio = async (type: SoundType, config: SoundConfig): Promise<HTMLAudioElement> => {
        if (!pool.has(type)) {
            pool.set(type, []);
        }

        const typePool = pool.get(type);
        if (!typePool) {
            throw new Error(`Pool not initialized for type: ${type}`);
        }

        // 再生可能な音声要素を探す
        for (const audio of typePool) {
            if (audio.ended || audio.paused) {
                return audio;
            }
        }

        // プールに空きがあれば新しい要素を作成
        if (typePool.length < maxPoolSize) {
            const audioUrl = await getAudioUrl(type);
            const audio = new Audio(audioUrl);
            audio.preload = config.preload ? "auto" : "metadata";
            typePool.push(audio);
            return audio;
        }

        // プールが満杯なら古い音声を停止して再利用
        const audio = typePool[0];
        audio.pause();
        audio.currentTime = 0;
        return audio;
    };

    const stopAll = (): void => {
        for (const typePool of pool.values()) {
            for (const audio of typePool) {
                audio.pause();
                audio.currentTime = 0;
            }
        }
    };

    const preload = async (type: SoundType, config: SoundConfig): Promise<void> => {
        try {
            const audio = await getAudio(type, config);

            if (audio.readyState >= 3) {
                // HAVE_FUTURE_DATA or better
                return;
            }

            return new Promise((resolve, reject) => {
                const onLoad = () => {
                    audio.removeEventListener("canplaythrough", onLoad);
                    audio.removeEventListener("error", onError);
                    resolve();
                };

                const onError = () => {
                    audio.removeEventListener("canplaythrough", onLoad);
                    audio.removeEventListener("error", onError);
                    reject(new Error(`Failed to load audio: ${config.path}`));
                };

                audio.addEventListener("canplaythrough", onLoad);
                audio.addEventListener("error", onError);

                // 読み込み開始
                audio.load();
            });
        } catch (error) {
            throw new Error(`Failed to preload ${type}: ${error}`);
        }
    };

    const cleanup = (): void => {
        // 生成されたBlobURLを解放
        for (const url of generatedUrls.values()) {
            revokeBlobURL(url);
        }
        generatedUrls.clear();

        // 音声プールをクリア
        stopAll();
        pool.clear();
    };

    return {
        getAudio,
        stopAll,
        preload,
        cleanup,
    };
}

/**
 * ブラウザベースの音声マネージャーを作成する関数
 */
export function createBrowserAudioManager(): AudioManager {
    const audioPool = createAudioPool();
    const state: AudioPlayerState = {
        isInitialized: false,
        volume: 50,
        isMuted: false,
        loadedSounds: new Set(),
    };

    const initialize = async (): Promise<void> => {
        if (state.isInitialized) {
            return;
        }

        try {
            // Web Audio APIの利用可能性チェック
            if (typeof Audio === "undefined") {
                throw new Error("Audio is not supported in this environment");
            }

            // プリロード対象の音声を読み込み
            const preloadPromises: Promise<void>[] = [];

            for (const [type, config] of Object.entries(SOUND_FILES) as [
                SoundType,
                SoundConfig,
            ][]) {
                if (config.preload) {
                    preloadPromises.push(preload(type));
                }
            }

            await Promise.all(preloadPromises);

            state.isInitialized = true;
        } catch (error) {
            console.warn("Audio initialization failed:", error);
            // エラーが発生しても初期化済みとして扱う（無音で動作）
            state.isInitialized = true;
        }
    };

    const play = async (type: SoundType, options: PlayOptions = {}): Promise<void> => {
        if (!state.isInitialized || state.isMuted) {
            return;
        }

        try {
            const config = SOUND_FILES[type];
            if (!config) {
                console.warn(`Unknown sound type: ${type}`);
                return;
            }
            const audio = await audioPool.getAudio(type, config);

            // 音量設定
            const volume = options.volume ?? volumeLevelToFloat(state.volume);
            const defaultVolume = volumeLevelToFloat(config.defaultVolume ?? 50);
            audio.volume = Math.min(volume, defaultVolume);

            // 再生速度設定
            if (options.playbackRate) {
                audio.playbackRate = options.playbackRate;
            }

            // 既存の再生を停止する場合
            if (options.interrupt && !audio.paused) {
                audio.pause();
                audio.currentTime = 0;
            }

            // 再生開始
            const playPromise = audio.play();
            if (playPromise) {
                await playPromise;
            }
        } catch (error) {
            // ユーザーのインタラクション前の自動再生エラーなどは無視
            console.error(`audioManager.play: Failed to play ${type}:`, error);
        }
    };

    const setVolume = (volume: VolumeLevel): void => {
        state.volume = volume;
    };

    const setMuted = (muted: boolean): void => {
        state.isMuted = muted;
    };

    const preload = async (type: SoundType): Promise<void> => {
        const config = SOUND_FILES[type];
        if (!config) {
            throw new Error(`Unknown sound type: ${type}`);
        }

        try {
            await audioPool.preload(type, config);
            state.loadedSounds.add(type);
        } catch (error) {
            console.warn(`Failed to preload ${type}:`, error);
        }
    };

    const stopAll = (): void => {
        audioPool.stopAll();
    };

    const getState = (): AudioPlayerState => {
        return { ...state, loadedSounds: new Set(state.loadedSounds) };
    };

    return {
        initialize,
        play,
        setVolume,
        setMuted,
        preload,
        stopAll,
        getState,
    };
}

/**
 * グローバル音声マネージャーインスタンス
 */
export const audioManager = createBrowserAudioManager();
