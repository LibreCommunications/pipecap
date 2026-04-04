export interface ScreenShareStreamOptions {
  fps?: number;
  audio?: boolean;
}

export interface ScreenShareStreamResult {
  /** MediaStream with video (and optionally audio) tracks. Pass to LiveKit. */
  stream: MediaStream;
  /** Stop capture and clean up all resources. */
  stop: () => void;
}

/**
 * Create a MediaStream from pipecap's raw frame/audio data.
 * Must be called AFTER `window.pipecap.startCapture()`.
 *
 * Returns a MediaStream suitable for LiveKit's publishTrack().
 */
export function createScreenShareStream(
  options?: ScreenShareStreamOptions,
): ScreenShareStreamResult;
