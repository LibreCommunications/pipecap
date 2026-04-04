import type { IpcMain, BrowserWindow } from 'electron';

/**
 * Register pipecap IPC handlers on the Electron main process.
 * Call this once in your main.ts after app.whenReady().
 */
export function setupPipecap(
  ipcMain: IpcMain,
  getWindow: () => BrowserWindow | null,
): void;
