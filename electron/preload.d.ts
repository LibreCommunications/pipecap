import type { ContextBridge, IpcRenderer } from 'electron';

/**
 * Expose pipecap API to the renderer via contextBridge.
 * Call this in your preload script.
 * Adds `window.pipecap` with picker, capture, and frame/audio listeners.
 */
export function exposePipecap(
  contextBridge: ContextBridge,
  ipcRenderer: IpcRenderer,
): void;
