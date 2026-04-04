/**
 * pipecap Electron preload bridge.
 *
 * Usage in your preload script:
 *
 *   const { exposePipecap } = require('@librecord/pipecap/electron/preload');
 *   exposePipecap(contextBridge, ipcRenderer);
 *
 * This exposes `window.pipecap` to the renderer.
 */

function exposePipecap(contextBridge, ipcRenderer) {
  contextBridge.exposeInMainWorld('pipecap', {
    available: () => ipcRenderer.invoke('pipecap:available'),
    showPicker: (sourceTypes) => ipcRenderer.invoke('pipecap:showPicker', sourceTypes),
    startCapture: (options) => ipcRenderer.invoke('pipecap:startCapture', options),
    stopCapture: () => ipcRenderer.invoke('pipecap:stopCapture'),
    isCapturing: () => ipcRenderer.invoke('pipecap:isCapturing'),

    onFrame: (callback) => {
      const handler = (_event, frame) => callback(frame);
      ipcRenderer.on('pipecap:frame', handler);
      return () => ipcRenderer.removeListener('pipecap:frame', handler);
    },

    onAudio: (callback) => {
      const handler = (_event, audio) => callback(audio);
      ipcRenderer.on('pipecap:audio', handler);
      return () => ipcRenderer.removeListener('pipecap:audio', handler);
    },
  });
}

module.exports = { exposePipecap };
