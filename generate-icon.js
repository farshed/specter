// Generates a simple 512x512 RGBA PNG app icon with no dependencies.
// A dark rounded square with a light "K" glyph drawn as a filled circle ring.
const fs = require("fs");
const zlib = require("zlib");

const SIZE = 512;
const bg = [29, 31, 35, 255]; // #1d1f23
const accent = [106, 163, 255, 255]; // #6aa3ff

function inRoundedRect(x, y, s, r) {
  const dx = Math.min(x, s - 1 - x);
  const dy = Math.min(y, s - 1 - y);
  if (dx >= r || dy >= r) return true;
  const cx = dx < r ? r : dx;
  const cy = dy < r ? r : dy;
  return (cx - dx) ** 2 + (cy - dy) ** 2 <= r * r;
}

const cx = SIZE / 2;
const cy = SIZE / 2;
const ringOuter = 150;
const ringInner = 92;

const raw = Buffer.alloc(SIZE * (SIZE * 4 + 1));
let p = 0;
for (let y = 0; y < SIZE; y++) {
  raw[p++] = 0; // filter type 0 (none) per scanline
  for (let x = 0; x < SIZE; x++) {
    let px = bg;
    if (!inRoundedRect(x, y, SIZE, 96)) {
      px = [0, 0, 0, 0]; // transparent outside the rounded square
    } else {
      const d = Math.hypot(x - cx, y - cy);
      if (d <= ringOuter && d >= ringInner) px = accent;
    }
    raw[p++] = px[0];
    raw[p++] = px[1];
    raw[p++] = px[2];
    raw[p++] = px[3];
  }
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const typeBuf = Buffer.from(type, "ascii");
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([typeBuf, data])) >>> 0, 0);
  return Buffer.concat([len, typeBuf, data, crc]);
}

const crcTable = (() => {
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
  for (let i = 0; i < buf.length; i++) c = crcTable[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

const sig = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(SIZE, 0);
ihdr.writeUInt32BE(SIZE, 4);
ihdr[8] = 8; // bit depth
ihdr[9] = 6; // color type RGBA
ihdr[10] = 0;
ihdr[11] = 0;
ihdr[12] = 0;
const idat = zlib.deflateSync(raw, { level: 9 });

const png = Buffer.concat([
  sig,
  chunk("IHDR", ihdr),
  chunk("IDAT", idat),
  chunk("IEND", Buffer.alloc(0)),
]);

fs.writeFileSync("app-icon.png", png);
console.log("wrote app-icon.png (" + png.length + " bytes)");
