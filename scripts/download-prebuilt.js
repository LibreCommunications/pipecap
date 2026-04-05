#!/usr/bin/env node

// Downloads the prebuilt .node binary from the GitHub release matching
// the package version. Skips if the binary already exists (local dev build).

const { existsSync, createWriteStream, unlinkSync } = require("fs");
const { join } = require("path");
const https = require("https");
const { execSync } = require("child_process");

const pkg = require("../package.json");
const version = `v${pkg.version}`;
const binaryName = "pipecap.linux-x64-gnu.node";
const binaryPath = join(__dirname, "..", binaryName);

if (existsSync(binaryPath)) {
  console.log(`pipecap: ${binaryName} already exists, skipping download`);
  process.exit(0);
}

if (process.platform !== "linux" || process.arch !== "x64") {
  console.log(`pipecap: skipping download (unsupported platform: ${process.platform}-${process.arch})`);
  process.exit(0);
}

const url = `https://github.com/LibreCommunications/pipecap/releases/download/${version}/${binaryName}`;
console.log(`pipecap: downloading ${url}`);

function download(url, dest, redirects = 0) {
  if (redirects > 5) {
    console.error("pipecap: too many redirects");
    process.exit(1);
  }

  https.get(url, (res) => {
    if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
      download(res.headers.location, dest, redirects + 1);
      return;
    }

    if (res.statusCode !== 200) {
      console.error(`pipecap: download failed (HTTP ${res.statusCode})`);
      console.error(`pipecap: you may need to build from source: npm run build`);
      process.exit(0); // don't fail install — optional dep
    }

    const file = createWriteStream(dest);
    res.pipe(file);
    file.on("finish", () => {
      file.close();
      console.log(`pipecap: downloaded ${binaryName}`);
    });
  }).on("error", (err) => {
    console.error(`pipecap: download error: ${err.message}`);
    try { unlinkSync(dest); } catch {}
    process.exit(0); // don't fail install
  });
}

download(url, binaryPath);
