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
let frameInterval = null;

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
    if (frameInterval) {
      clearInterval(frameInterval);
      frameInterval = null;
    }
    try { pc.stopCapture(); } catch { /* ignore */ }

    pc.startCapture(options);

    // Pump frames to renderer at the requested fps
    const fps = options.fps || 30;
    const win = getWindow();
    if (win && !win.isDestroyed()) {
      frameInterval = setInterval(() => {
        const w = getWindow();
        if (!w || w.isDestroyed()) {
          clearInterval(frameInterval);
          frameInterval = null;
          return;
        }
        try {
          const frame = pc.readFrame();
          if (frame) {
            w.webContents.send('pipecap:frame', {
              width: frame.width,
              height: frame.height,
              data: frame.data,
            });
          }
          if (options.audio) {
            const audio = pc.readAudio();
            if (audio) {
              w.webContents.send('pipecap:audio', {
                channels: audio.channels,
                sampleRate: audio.sampleRate,
                data: audio.data,
              });
            }
          }
        } catch {
          // Capture may have stopped
        }
      }, Math.round(1000 / fps));
    }

    return true;
  });

  ipcMain.handle('pipecap:stopCapture', () => {
    if (frameInterval) {
      clearInterval(frameInterval);
      frameInterval = null;
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
