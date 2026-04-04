# pipecap

Native PipeWire screen capture for Electron on Linux. Shows the system's xdg-desktop-portal picker (KDE, GNOME, Sway, etc.) and returns raw video frames from the selected source.

Built with Rust and [napi-rs](https://napi.rs). Respects Wayland's security model — all capture requires explicit user consent through the portal.

## Why

Electron's `desktopCapturer.getSources()` cannot see native Wayland windows. This module bypasses that limitation by calling xdg-desktop-portal directly via D-Bus, then consuming the PipeWire video stream natively.

## Install

```bash
npm install @librecord/pipecap
```

Requires PipeWire and xdg-desktop-portal running on the system (standard on modern Linux desktops).

## Usage

```typescript
import { showPicker, startCapture, readFrame, stopCapture } from '@librecord/pipecap';

// 1. Show native screen picker (source_types: 1=monitors, 2=windows, 3=both)
const streams = await showPicker(3);
if (!streams) process.exit(0); // User cancelled

const { nodeId, width, height } = streams[0];

// 2. Start capturing frames from the PipeWire node
startCapture(nodeId, width, height);

// 3. Read frames in a loop
setInterval(() => {
  const frame = readFrame();
  if (frame) {
    console.log(`${frame.width}x${frame.height}, ${frame.data.length} bytes (RGBA)`);
  }
}, 33); // ~30 fps

// 4. Stop when done
// stopCapture();
```

## API

### `showPicker(sourceTypes: number): Promise<PortalStream[] | null>`

Shows the native xdg-desktop-portal screen/window picker. Returns selected streams or `null` if cancelled.

### `startCapture(nodeId: number, width: number, height: number): void`

Starts capturing video frames from a PipeWire node. The `nodeId` must come from `showPicker()`.

### `readFrame(): Frame | null`

Returns the latest captured frame as `{ width, height, data: Buffer }` (RGBA pixels), or `null` if no frame is available yet.

### `stopCapture(): void`

Stops capturing and releases PipeWire resources.

## Building from source

```bash
# Prerequisites: Rust toolchain, PipeWire dev headers, Node.js 20+
sudo pacman -S pipewire libpipewire   # Arch
sudo apt install libpipewire-0.3-dev  # Debian/Ubuntu

npm install
npm run build
```

## Acknowledgements

This project is built on the work of:

- [**ashpd**](https://github.com/bilelmoussaoui/ashpd) by Bilal Elmoussaoui — Rust wrapper for xdg-desktop-portal that makes the portal API accessible and safe. This is what powers the native screen picker.
- [**pipewire-rs**](https://gitlab.freedesktop.org/pipewire/pipewire-rs) by Tom Wagner and Guillaume Desmottes — Rust bindings for PipeWire that make it possible to consume video streams natively.
- [**napi-rs**](https://napi.rs) by LongYinan — the bridge that turns Rust into a Node.js native addon.

## License

[AGPL-3.0](LICENSE)
