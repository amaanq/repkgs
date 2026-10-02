// Fills $BUN_INSTALL_CACHE_DIR from a fetch.bunDeps output so `bun install --offline` resolves every
// package, linking <cache>/<name>@<version>@@@1 to <deps>/p/<name>@<version>. bun does not spell
// pre-release and build parts literally but as their Wyhash11, the variant in zig 0.11's
// std.hash.Wyhash (src/install/PackageManager/PackageManagerDirectories.zig), so typescript@5.7.0-beta
// lives in typescript@5.7.0-50d49428a66e9f35@@@1. Computing that hash is why this is TypeScript.
//
//   bun bun-cache.ts <deps dir> <cache dir>
//   bun bun-cache.ts --print <name@version>
import { mkdirSync, symlinkSync } from "node:fs";
import { dirname, join } from "node:path";

const MASK = 0xffffffffffffffffn;
const P0 = 0xa0761d6478bd642fn;
const P1 = 0xe7037ed1a0b428dbn;
const P2 = 0x8ebc6af09c88c6e3n;
const P3 = 0x589965cc75374cc3n;
const P4 = 0x1d8e4e27c47d124fn;

function littleEndian(bytes: Uint8Array, offset: number, width: number): bigint {
  let value = 0n;
  for (let i = width - 1; i >= 0; i--) {
    value = (value << 8n) | BigInt(bytes[offset + i]);
  }
  return value;
}

function multiplyFold(a: bigint, b: bigint): bigint {
  const product = (a & MASK) * (b & MASK);
  return (product ^ (product >> 64n)) & MASK;
}

// zig reads a full 8-byte remainder word as two little-endian halves, the first one high
function remainderWord(bytes: Uint8Array, offset: number): bigint {
  return (littleEndian(bytes, offset, 4) << 32n) | littleEndian(bytes, offset + 4, 4);
}

// a partial word of 1 to 7 bytes, read as 4, 2 and 1 byte pieces with the first piece highest
function partialWord(bytes: Uint8Array, offset: number, length: number): bigint {
  let value = 0n;
  let position = offset;
  for (const piece of [4, 2, 1]) {
    if ((length & piece) === 0) continue;
    value = (value << BigInt(8 * piece)) | littleEndian(bytes, position, piece);
    position += piece;
  }
  return value;
}

function wyhash11(text: string): bigint {
  const bytes = new TextEncoder().encode(text);
  let seed = 0n;
  let offset = 0;
  const round = (a: bigint, b: bigint, c: bigint, d: bigint): bigint =>
    multiplyFold(a ^ seed ^ P0, b ^ seed ^ P1) ^ multiplyFold(c ^ seed ^ P2, d ^ seed ^ P3);

  while (offset + 32 <= bytes.length) {
    seed = round(
      littleEndian(bytes, offset, 8),
      littleEndian(bytes, offset + 8, 8),
      littleEndian(bytes, offset + 16, 8),
      littleEndian(bytes, offset + 24, 8),
    );
    offset += 32;
  }

  // the last 0 to 31 bytes as up to four words, a missing word standing in as P4
  const left = bytes.length - offset;
  const word = (index: number): bigint => {
    const available = left - 8 * index;
    if (available >= 8) return remainderWord(bytes, offset + 8 * index);
    if (available > 0) return partialWord(bytes, offset + 8 * index, available);
    return P4;
  };
  if (left > 16) {
    seed = round(word(0), word(1), word(2), word(3));
  } else if (left > 0) {
    seed = multiplyFold(word(0) ^ seed ^ P0, word(1) ^ seed ^ P1);
  }
  return multiplyFold(seed ^ BigInt(bytes.length), P4);
}

function hex(value: bigint): string {
  return value.toString(16).padStart(16, "0");
}

// the directory bun looks up for "name@1.2.3-pre+build"
export function cacheDirName(id: string): string {
  const at = id.lastIndexOf("@");
  if (at <= 0) return `${id}@@@1`;
  const name = id.slice(0, at);
  const [core, build] = id.slice(at + 1).split("+", 2);
  const dash = core.indexOf("-");
  let version = dash < 0 ? core : core.slice(0, dash);
  if (dash >= 0) version += `-${hex(wyhash11(core.slice(dash + 1)))}`;
  if (build !== undefined) version += `+${hex(wyhash11(build)).toUpperCase()}`;
  return `${name}@${version}@@@1`;
}

if (import.meta.main) {
  const [first, second] = process.argv.slice(2);
  if (first === "--print") {
    console.log(cacheDirName(second));
  } else {
    const index: { id: string; dir: string }[] = await Bun.file(join(first, "index.json")).json();
    for (const { id, dir } of index) {
      const link = join(second, cacheDirName(id));
      mkdirSync(dirname(link), { recursive: true });
      symlinkSync(join(first, dir), link);
    }
  }
}
