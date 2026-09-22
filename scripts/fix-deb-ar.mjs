#!/usr/bin/env node
// Repack .deb bundles whose `ar` member headers are malformed.
//
// tauri-bundler writes the builder's uid/gid into the fixed-width uid/gid
// fields (6 chars each) of the `ar` header without truncating. On machines
// where the uid exceeds 6 digits (e.g. AD domain accounts), the fields
// overflow, every header boundary shifts, and dpkg/apt reject the package:
//   E: Invalid archive member header
// Upstream: https://github.com/tauri-apps/tauri/issues/9558 (open)
//
// This script rewrites each member header with well-formed fields
// (uid/gid 0) while preserving member payloads byte-for-byte. Archives
// that are already valid are left untouched.
//
// Usage: node scripts/fix-deb-ar.mjs <deb> [<deb>...]

import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';

const AR_MAGIC = '!<arch>\n';
const HEADER_END = '`\n';

// A valid ar header is exactly 60 bytes: the "`\n" terminator must land at
// offset 58. The malformed archives have it further out (uid/gid overflow).
function isMalformed(buf) {
  if (buf.subarray(0, 8).toString() !== AR_MAGIC) {
    throw new Error('not an ar archive');
  }
  let pos = 8;
  while (pos < buf.length) {
    if (buf[pos + 58] !== 0x60 || buf[pos + 59] !== 0x0a) return true;
    const size = Number(buf.subarray(pos + 48, pos + 58).toString().trim());
    pos += 60 + size + (size % 2);
  }
  return false;
}

// Split members the lenient way: a header is everything up to its "`\n"
// terminator, and the size is its last whitespace-separated field. Works
// for both valid and uid/gid-overflowed headers.
function parseMembers(buf) {
  const members = [];
  let pos = 8;
  while (pos < buf.length) {
    const end = buf.indexOf(HEADER_END, pos);
    if (end === -1) throw new Error(`truncated header at offset ${pos}`);
    const fields = buf.subarray(pos, end).toString('ascii').trim().split(/\s+/);
    const name = fields[0].replace(/\/$/, '');
    if (name.startsWith('#1/') || name === '//' || name === '/SYM64/') {
      throw new Error(`unsupported ar member name form: ${name}`);
    }
    const size = Number(fields[fields.length - 1]);
    if (!Number.isFinite(size) || size < 0) {
      throw new Error(`bad member size in header at offset ${pos}`);
    }
    const start = end + 2;
    members.push({ name, data: buf.subarray(start, start + size) });
    pos = start + size + (size % 2);
  }
  return members;
}

function header(name, size) {
  const h =
    `${name}/`.padEnd(16, ' ') +
    '0'.padEnd(12, ' ') + // mtime — normalized for reproducibility
    '0'.padEnd(6, ' ') + // uid
    '0'.padEnd(6, ' ') + // gid
    '100644  ' +
    String(size).padEnd(10, ' ') +
    HEADER_END;
  if (h.length !== 60) throw new Error(`bad header length for ${name}`);
  return Buffer.from(h, 'ascii');
}

let failed = false;
for (const file of process.argv.slice(2)) {
  if (!existsSync(file)) continue; // unmatched glob passed literally
  try {
    const buf = readFileSync(file);
    if (!isMalformed(buf)) {
      console.log(`${file}: already valid`);
      continue;
    }
    const members = parseMembers(buf);
    const chunks = [Buffer.from(AR_MAGIC)];
    for (const m of members) {
      chunks.push(header(m.name, m.data.length), m.data);
      if (m.data.length % 2) chunks.push(Buffer.from('\n'));
    }
    const out = Buffer.concat(chunks);
    if (isMalformed(out)) throw new Error('repacked archive still malformed');
    writeFileSync(file, out);
    // Independent sanity check via binutils ar.
    const listing = execFileSync('ar', ['t', file], { encoding: 'utf8' }).trim();
    console.log(`${file}: repacked (${members.length} members: ${listing})`);
  } catch (err) {
    failed = true;
    console.error(`${file}: ${err.message}`);
  }
}
process.exit(failed ? 1 : 0);
