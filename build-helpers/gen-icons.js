// Builds src-tauri/icons/icon.ico from branding-src/gen/*.png.
// ICO entries embed PNGs directly (supported by Windows Vista+; the format
// every modern app ships). Sizes: 16,24,32,48,64,128,256.
// Also copies the 32px PNG to src-tauri/icons/icon-32.png (tray icon).
// Usage: node build-helpers/gen-icons.js   (run resize-icons.ps1 first)

const fs = require("fs");
const path = require("path");

const genDir = path.join(__dirname, "..", "branding-src", "gen");
const outDir = path.join(__dirname, "..", "src-tauri", "icons");
const SIZES = [16, 24, 32, 48, 64, 128, 256];

const pngs = SIZES.map((s) => fs.readFileSync(path.join(genDir, `${s}.png`)));

// ICO container: 6-byte header + N 16-byte directory entries + payloads.
const header = Buffer.alloc(6);
header.writeUInt16LE(0, 0); // reserved
header.writeUInt16LE(1, 2); // type: icon
header.writeUInt16LE(SIZES.length, 4);

let offset = 6 + SIZES.length * 16;
const dirs = SIZES.map((s, i) => {
  const d = Buffer.alloc(16);
  d.writeUInt8(s >= 256 ? 0 : s, 0); // width (0 = 256)
  d.writeUInt8(s >= 256 ? 0 : s, 1); // height
  d.writeUInt8(0, 2); // palette
  d.writeUInt8(0, 3); // reserved
  d.writeUInt16LE(1, 4); // color planes
  d.writeUInt16LE(32, 6); // bpp
  d.writeUInt32LE(pngs[i].length, 8);
  d.writeUInt32LE(offset, 12);
  offset += pngs[i].length;
  return d;
});

fs.mkdirSync(outDir, { recursive: true });
fs.writeFileSync(path.join(outDir, "icon.ico"), Buffer.concat([header, ...dirs, ...pngs]));
fs.copyFileSync(path.join(genDir, "32.png"), path.join(outDir, "icon-32.png"));
console.log(`wrote icon.ico (${SIZES.join(",")}) and icon-32.png to ${outDir}`);
