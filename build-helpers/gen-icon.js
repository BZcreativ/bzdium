// Generates src-tauri/icons/icon.ico (16x16 + 32x32 BMP-in-ICO) and
// src-tauri/icons/icon-32.png for bzdium.
// Solid purple (#6C4FBB) background with a white "B" drawn from a 5x7 bitmap.
// Usage: node build-helpers/gen-icon.js

const fs = require("fs");
const path = require("path");
const zlib = require("zlib");

const GLYPH = ["11110", "10001", "10001", "11110", "10001", "10001", "11110"];
const BG = [0x6c, 0x4f, 0xbb]; // RGB
const FG = [0xff, 0xff, 0xff];

function render(size) {
  // pixels: RGBA rows top->bottom
  const px = new Uint8Array(size * size * 4);
  const scale = Math.max(1, Math.floor(size / 11));
  const gw = 5 * scale;
  const gh = 7 * scale;
  const ox = Math.floor((size - gw) / 2);
  const oy = Math.floor((size - gh) / 2);
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const gx = Math.floor((x - ox) / scale);
      const gy = Math.floor((y - oy) / scale);
      const on =
        gx >= 0 && gx < 5 && gy >= 0 && gy < 7 && GLYPH[gy][gx] === "1";
      const c = on ? FG : BG;
      const i = (y * size + x) * 4;
      px[i] = c[0];
      px[i + 1] = c[1];
      px[i + 2] = c[2];
      px[i + 3] = 255;
    }
  }
  return px;
}

function bmpEntry(size) {
  const px = render(size);
  const maskRowBytes = Math.ceil(size / 32) * 4;
  const maskSize = maskRowBytes * size;
  const header = Buffer.alloc(40);
  header.writeUInt32LE(40, 0); // BITMAPINFOHEADER size
  header.writeInt32LE(size, 4);
  header.writeInt32LE(size * 2, 8); // XOR + AND mask
  header.writeUInt16LE(1, 12); // planes
  header.writeUInt16LE(32, 14); // bpp
  header.writeUInt32LE(0, 16); // BI_RGB
  header.writeUInt32LE(size * size * 4 + maskSize, 20);
  const xor = Buffer.alloc(size * size * 4);
  // bottom-up BGRA
  for (let y = 0; y < size; y++) {
    const srcRow = size - 1 - y;
    for (let x = 0; x < size; x++) {
      const s = (srcRow * size + x) * 4;
      const d = (y * size + x) * 4;
      xor[d] = px[s + 2]; // B
      xor[d + 1] = px[s + 1]; // G
      xor[d + 2] = px[s]; // R
      xor[d + 3] = px[s + 3]; // A
    }
  }
  const mask = Buffer.alloc(maskSize); // all zero = fully opaque
  return Buffer.concat([header, xor, mask]);
}

function makeIco(sizes) {
  const entries = sizes.map((s) => bmpEntry(s));
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(sizes.length, 4);
  let offset = 6 + sizes.length * 16;
  const dirs = sizes.map((s, i) => {
    const d = Buffer.alloc(16);
    d.writeUInt8(s >= 256 ? 0 : s, 0);
    d.writeUInt8(s >= 256 ? 0 : s, 1);
    d.writeUInt8(0, 2);
    d.writeUInt8(0, 3);
    d.writeUInt16LE(1, 4);
    d.writeUInt16LE(32, 6);
    d.writeUInt32LE(entries[i].length, 8);
    d.writeUInt32LE(offset, 12);
    offset += entries[i].length;
    return d;
  });
  return Buffer.concat([header, ...dirs, ...entries]);
}

// --- minimal PNG encoder (RGBA8, filter 0) ---
const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();
function crc32(buf) {
  let c = 0xffffffff;
  for (const b of buf) c = CRC_TABLE[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}
function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32LE(0, 0);
  len.writeUInt32BE(data.length, 0);
  const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body), 0);
  return Buffer.concat([len, body, crc]);
}
function makePng(size) {
  const px = render(size);
  const raw = Buffer.alloc(size * (size * 4 + 1));
  for (let y = 0; y < size; y++) {
    raw[y * (size * 4 + 1)] = 0;
    Buffer.from(px.buffer, y * size * 4, size * 4).copy(
      raw,
      y * (size * 4 + 1) + 1
    );
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // RGBA
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", zlib.deflateSync(raw, { level: 9 })),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

const outDir = path.join(__dirname, "..", "src-tauri", "icons");
fs.mkdirSync(outDir, { recursive: true });
fs.writeFileSync(path.join(outDir, "icon.ico"), makeIco([16, 32]));
fs.writeFileSync(path.join(outDir, "icon-32.png"), makePng(32));
console.log("wrote icon.ico (16,32) and icon-32.png to", outDir);
