/**
 * pipecap Electron main-process helper.
 *
 * Usage in your Electron main process:
 *
 *   const { setupPipecap } = require('@librecord/pipecap/electron/main');
 *   setupPipecap(ipcMain, () => mainWindow);
 *
 * This registers IPC handlers that the renderer calls via the preload bridge.
 */

let pipecap = null;
let audioInterval = null;

function loadPipecap() {
  if (pipecap) return pipecap;
  try {
    pipecap = require('@librecord/pipecap');
    return pipecap;
  } catch (e) {
    console.warn('pipecap: native module not available', e.message);
    return null;
  }
}

/**
 * Register pipecap IPC handlers on the main process.
 *
 * @param {Electron.IpcMain} ipcMain
 * @param {() => Electron.BrowserWindow | null} getWindow
 */
function setupPipecap(ipcMain, getWindow) {
  ipcMain.handle('pipecap:available', () => {
    return !!loadPipecap();
  });

  ipcMain.handle('pipecap:showPicker', async (_e, sourceTypes) => {
    const pc = loadPipecap();
    if (!pc) return null;
    return pc.showPicker(sourceTypes ?? 3);
  });

  ipcMain.handle('pipecap:startCapture', (_e, options) => {
    const pc = loadPipecap();
    if (!pc) return false;

    // Stop any existing capture
    if (audioInterval) {
      clearInterval(audioInterval);
      audioInterval = null;
    }
    try { pc.stopCapture(); } catch { /* ignore */ }

    const shmInfo = pc.startCapture(options);

    // Video frames are delivered via shared memory (shmInfo.shmPath),
    // not IPC — the renderer reads them directly.
    // Audio chunks are pumped to the renderer via IPC.
    if (options.audio) {
      const win = getWindow();
      if (win && !win.isDestroyed()) {
        audioInterval = setInterval(() => {
          const w = getWindow();
          if (!w || w.isDestroyed()) {
            clearInterval(audioInterval);
            audioInterval = null;
            return;
          }
          try {
            const audio = pc.readAudio();
            if (audio) {
              w.webContents.send('pipecap:audio', {
                channels: audio.channels,
                sampleRate: audio.sampleRate,
                data: audio.data,
              });
            }
          } catch {
            // Capture may have stopped
          }
        }, 20); // ~50Hz polling for audio chunks
      }
    }

    return shmInfo;
  });

  ipcMain.handle('pipecap:stopCapture', () => {
    if (audioInterval) {
      clearInterval(audioInterval);
      audioInterval = null;
    }
    const pc = loadPipecap();
    if (pc) {
      try { pc.stopCapture(); } catch { /* ignore */ }
    }
  });

  ipcMain.handle('pipecap:isCapturing', () => {
    const pc = loadPipecap();
    return pc ? pc.isCapturing() : false;
  });
}

module.exports = { setupPipecap };
