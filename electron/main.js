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
 * @param {{ transformStartOptions?: (options: Record<string, unknown>) => Record<string, unknown> }} [opts]
 *   `transformStartOptions` runs in the main process on every `startCapture`
 *   request — use it to inject things the renderer should not see, e.g.
 *   `{ excludePids: app.getAppMetrics().map(m => m.pid) }` so the host app
 *   never appears in its own audio share.
 */
function setupPipecap(ipcMain, getWindow, opts = {}) {
  const transformStartOptions = opts.transformStartOptions || ((o) => o);
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

    // Stop any prior audio polling. We deliberately do NOT call
    // pc.stopCapture() here: the native start_capture already replaces any
    // existing video/audio capturer, and stopCapture would also close the
    // PortalHandle that showPicker just stored — the very fd we're about
    // to consume.
    if (audioInterval) {
      clearInterval(audioInterval);
      audioInterval = null;
    }

    const finalOptions = transformStartOptions(options) || options;
    const shmInfo = pc.startCapture(finalOptions);

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
