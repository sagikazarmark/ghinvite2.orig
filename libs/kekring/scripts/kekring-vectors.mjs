#!/usr/bin/env node
// Independent re-derivation of the kekring v1 vectors (`docs/kekring-format.md`) with Node's
// OpenSSL bindings only: HKDF-SHA256, SHA-256, `id-aes256-wrap` (RFC 3394) and AES-256-GCM. It
// shares no code with the Rust crate; it is written from the format document.
//
//   node scripts/kekring-vectors.mjs          # writes tests/vectors/kekring-v1.json
//   node scripts/kekring-vectors.mjs --check  # fails unless the committed file is identical
//
// It also checks its canonical AAD encoder against upstream's tests/vectors/aad_v1.json.
import { createCipheriv, createHash, hkdfSync } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';

const here = (p) => fileURLToPath(new URL(p, import.meta.url));
const OUT = here('../tests/vectors/kekring-v1.json');
const AAD_VECTORS = here('../tests/vectors/aad_v1.json');

// The wire labels.
const PREFIX = 'kekring1';
const MAGIC = Buffer.from('krw1');
const SALT = Buffer.from('kekring/v1/salt');
const KEK_LABEL = Buffer.from('kekring/v1/kek');
const KID_LABEL = Buffer.from('kekring/v1/kid');
const FP_LABEL = Buffer.from('kekring/v1/fp');
const AAD_LABEL = Buffer.from('anykms/aad/v1\0', 'latin1');
const ENVELOPE_INFO = Buffer.from('anykms/envelope/v1');

const u32 = (v) => { const b = Buffer.alloc(4); b.writeUInt32BE(v); return b; };
const u16 = (v) => { const b = Buffer.alloc(2); b.writeUInt16BE(v); return b; };
const u64 = (v) => { const b = Buffer.alloc(8); b.writeBigUInt64BE(BigInt(v)); return b; };
const hex = (b) => Buffer.from(b).toString('hex');

// Derivation: HKDF-SHA256(salt, root) expanded with `label ‖ u8 len ‖ name ‖ u32be version`.
const info = (label, name, v) => Buffer.concat([label, Buffer.from([Buffer.byteLength(name)]), Buffer.from(name), u32(v)]);
const hkdf = (root, label, name, v, len) => Buffer.from(hkdfSync('sha256', root, SALT, info(label, name, v), len));
const kek = (name, v, root) => hkdf(root, KEK_LABEL, name, v, 32);
const kid = (name, v, root) => hkdf(root, KID_LABEL, name, v, 8);

// A keyring as plain data: { name, current, hwm, gens: [[version, root]] } (ascending).
const ring = (name, current, gens, hwm = Math.max(...gens.map(([v]) => v))) => ({ name, current, hwm, gens });
const descriptor = (r) => [
  PREFIX, `name=${r.name}`, `current=${r.current}`, `hwm=${r.hwm}`,
  ...r.gens.map(([v, root]) => `k${v}=${root.toString('base64url')}.${hex(kid(r.name, v, root))}`),
].join(';');
const fingerprint = (r) => {
  const h = createHash('sha256');
  h.update(FP_LABEL); h.update(Buffer.from([Buffer.byteLength(r.name)])); h.update(r.name);
  h.update(u32(r.current)); h.update(u32(r.hwm));
  for (const [v, root] of r.gens) { h.update(u32(v)); h.update(kid(r.name, v, root)); }
  return hex(h.digest().subarray(0, 8));
};

// AES-256-KW (RFC 3394, default IV) of a 32-byte DEK; the blob prefixes magic, version and kid.
const keyWrap = (key, dek) => {
  const c = createCipheriv('id-aes256-wrap', key, Buffer.from('A6A6A6A6A6A6A6A6', 'hex'));
  return Buffer.concat([c.update(dek), c.final()]);
};
// RFC 3394 §4.6: 256 bits of key data with a 256-bit KEK, so `id-aes256-wrap` is that primitive.
assert.equal(
  hex(keyWrap(Buffer.from('000102030405060708090A0B0C0D0E0F101112131415161718191A1B1C1D1E1F', 'hex'),
    Buffer.from('00112233445566778899AABBCCDDEEFF000102030405060708090A0B0C0D0E0F', 'hex'))),
  '28c9f404c4b810f4cbccb35cfb87f8263f5786e2d80ed326cbc7f0e71a99f43bfb988b9b7a02dd21',
);
const blob = (r, v, dek) => {
  const root = new Map(r.gens).get(v);
  return Buffer.concat([MAGIC, u32(v), kid(r.name, v, root), keyWrap(kek(r.name, v, root), dek)]);
};

// Canonical AAD v1 (anykms docs/aad-format.md): label ‖ u16 n ‖ (u16 klen ‖ key ‖ u16 vlen ‖ value)*
// sorted by key bytes, `purpose` counted as a field. Valid inputs only (rules are checked in Rust).
const aad = (purpose, fields) => {
  const all = [...fields, ['purpose', purpose]].map(([k, v]) => [Buffer.from(k), Buffer.from(v)]);
  all.sort((a, b) => Buffer.compare(a[0], b[0]));
  return Buffer.concat([AAD_LABEL, u16(all.length), ...all.flatMap(([k, v]) => [u16(k.length), k, u16(v.length), v])]);
};

// anykms envelope v1 with a fixed DEK and nonce; `wrapped` is the key-wrapping slot.
const pad = (pt, block) => {
  if (!block) return pt;
  const len = (Math.floor(pt.length / block) + 1) * block;
  return Buffer.concat([pt, Buffer.from([0x80]), Buffer.alloc(len - pt.length - 1)]);
};
const envelope = ({ kekId, wrapped, dek, nonce, plaintext, userAad, block }) => {
  const okm = Buffer.from(hkdfSync('sha256', dek, Buffer.alloc(0), ENVELOPE_INFO, 64));
  const header = Buffer.concat([Buffer.from('AKE'), Buffer.from([1, block ? 0x04 : 0x00]), okm.subarray(32), nonce]);
  const c = createCipheriv('aes-256-gcm', okm.subarray(0, 32), nonce);
  c.setAAD(Buffer.concat([header, u64(userAad.length), userAad]));
  const body = Buffer.concat([c.update(pad(plaintext, block)), c.final(), c.getAuthTag()]);
  const id = Buffer.from(kekId);
  return Buffer.concat([header, Buffer.from([id.length]), id, u16(wrapped.length), wrapped, body]);
};

// Inputs.
const seq = (start) => Buffer.from(Array.from({ length: 32 }, (_, i) => (start + i) & 0xff));
const R1 = seq(0x01), R2 = seq(0x21), R3 = seq(0x41), R4 = seq(0x61);
const DEK = Buffer.alloc(32, 0x42), DEK2 = Buffer.alloc(32, 0x24);
const NONCE = Buffer.alloc(12, 0x19);
const LONG_NAME = `n${'0123456789'.repeat(6)}abc`;
assert.equal(LONG_NAME.length, 64);

const rings = {
  single: ring('vectors', 1, [[1, R1]]),
  added: ring('vectors', 1, [[1, R1], [2, R2]]),
  promoted: ring('vectors', 2, [[1, R1], [2, R2]]),
  retired: ring('vectors', 2, [[2, R2]]),
  rolled_back: ring('vectors', 1, [[1, R1]], 2),
  gap: ring('vectors', 3, [[1, R1], [3, R3]]),
  other_name: ring('other', 1, [[1, R1]]),
  long_name: ring(LONG_NAME, 1, [[1, R1]]),
};
// The largest descriptor the grammar allows: a 64-byte name, 64 generations, 10-digit versions.
const maxGens = Array.from({ length: 64 }, (_, i) => [4294967295 - 63 + i, createHash('sha256').update(`root-${i}`).digest()]);
rings.largest = ring(LONG_NAME, 4294967295, maxGens);

const derivation = [
  ['vectors', 1, R1], ['vectors', 2, R2], ['vectors', 2, R1], ['other', 1, R1], [LONG_NAME, 1, R1], ['vectors', 4294967295, R4],
].map(([name, version, root]) => ({ name, version, root_hex: hex(root), kek_hex: hex(kek(name, version, root)), kid_hex: hex(kid(name, version, root)) }));

const descriptors = Object.entries(rings).map(([id, r]) => ({
  id, descriptor: descriptor(r), name: r.name, current: r.current, hwm: r.hwm,
  versions: r.gens.map(([v]) => v), kids_hex: r.gens.map(([v, root]) => hex(kid(r.name, v, root))), fingerprint: fingerprint(r),
}));
const largest = descriptors.find((d) => d.id === 'largest').descriptor;
assert.equal(largest.length, 4784);
assert.ok(largest.length + 1 <= 5120, 'worst-case descriptor fits a 5 KiB secret');
descriptors.push({ ...descriptors[0], id: 'single_trailing_newline', descriptor: `${descriptors[0].descriptor}\n` });

// Negative descriptor vectors, one or more per parser rule. Each names the rule's detail code.
const d1 = descriptor(rings.single), d2 = descriptor(rings.added);
const k1 = d1.split(';')[4], k2 = d2.split(';')[5];
const root1 = R1.toString('base64url'), kid1 = hex(kid('vectors', 1, R1));
const invalid_descriptors = [
  ['too_long', d1 + ';'.repeat(8193 - d1.length), 'length'],
  ['space', d1.replace(';current', '; current'), 'charset'],
  ['crlf', `${d1}\r\n`, 'charset'],
  ['two_newlines', `${d1}\n\n`, 'charset'],
  ['tab', `${d1}\t`, 'charset'],
  ['non_ascii', d1.replace('vectors', 'véctors'), 'charset'],
  ['old_prefix', d1.replace('kekring1', 'akr1'), 'prefix'],
  ['next_prefix', d1.replace('kekring1', 'kekring2'), 'prefix'],
  ['empty', '', 'prefix'],
  ['fields_reordered', d1.replace('name=vectors;current=1', 'current=1;name=vectors'), 'field_order'],
  ['hwm_missing', d1.replace(';hwm=1', ''), 'field_order'],
  ['name_missing', d1.replace(';name=vectors', ''), 'field_order'],
  ['unknown_field', `${d1};extra=x`, 'field_order'],
  ['name_repeated', `${d1};name=vectors`, 'field_order'],
  ['trailing_separator', `${d1};`, 'field_order'],
  ['uppercase_name', d1.replace('name=vectors', 'name=Vectors'), 'name'],
  ['dotted_name', d1.replace('name=vectors', 'name=p1.email'), 'name'],
  ['empty_name', d1.replace('name=vectors', 'name='), 'name'],
  ['leading_hyphen_name', d1.replace('name=vectors', 'name=-vectors'), 'name'],
  ['name_too_long', d1.replace('name=vectors', `name=${LONG_NAME}x`), 'name'],
  ['current_padded', d1.replace('current=1', 'current=01'), 'version'],
  ['current_zero', d1.replace('current=1', 'current=0'), 'version'],
  ['current_signed', d1.replace('current=1', 'current=+1'), 'version'],
  ['current_overflow', d1.replace('current=1', 'current=4294967296'), 'version'],
  ['hwm_empty', d1.replace('hwm=1', 'hwm='), 'version'],
  ['generation_zero', d1.replace(';k1=', ';k0='), 'version'],
  ['generation_padded', d1.replace(';k1=', ';k01='), 'version'],
  ['generation_overflow', d1.replace(';k1=', ';k4294967296=').replace('current=1', 'current=4294967296'), 'version'],
  ['generation_not_k', d1.replace(';k1=', ';key1='), 'version'],
  ['generations_descending', `${PREFIX};name=vectors;current=1;hwm=2;${k2};${k1}`, 'generation_order'],
  ['generation_repeated', `${d1};${k1}`, 'generation_order'],
  ['no_generations', `${PREFIX};name=vectors;current=1;hwm=1`, 'generation_count'],
  ['sixty_five_generations', [PREFIX, 'name=vectors', 'current=1', 'hwm=65', ...Array.from({ length: 65 }, (_, i) => {
    const root = createHash('sha256').update(`g-${i}`).digest();
    return `k${i + 1}=${root.toString('base64url')}.${hex(kid('vectors', i + 1, root))}`;
  })].join(';'), 'generation_count'],
  ['root_short', d1.replace(root1, root1.slice(1)), 'root'],
  ['root_padded', d1.replace(root1, `${root1}=`), 'root'],
  ['root_standard_alphabet', d1.replace(root1, root1.replace(/-/g, '+').replace(/_/g, '/').slice(0, 42) + '+'), 'root'],
  ['root_noncanonical_bits', d1.replace(root1, root1.slice(0, 42) + 'B'), 'root'],
  ['root_bad_char', d1.replace(root1, `!${root1.slice(1)}`), 'root'],
  ['kid_missing', d1.replace(`.${kid1}`, ''), 'kid'],
  ['kid_uppercase', d1.replace(kid1, kid1.toUpperCase()), 'kid'],
  ['kid_short', d1.replace(kid1, kid1.slice(1)), 'kid'],
  ['kid_long', d1.replace(kid1, `${kid1}0`), 'kid'],
  ['kid_mismatch', d1.replace(kid1, '0000000000000000'), 'kid_mismatch'],
  ['kid_of_other_name', d1.replace(kid1, hex(kid('other', 1, R1))), 'kid_mismatch'],
  ['zero_root', `${PREFIX};name=vectors;current=1;hwm=1;k1=${Buffer.alloc(32).toString('base64url')}.${hex(kid('vectors', 1, Buffer.alloc(32)))}`, 'zero_root'],
  ['duplicate_root', `${PREFIX};name=vectors;current=1;hwm=2;${k1};k2=${root1}.${hex(kid('vectors', 2, R1))}`, 'duplicate_root'],
  ['current_not_listed', d1.replace('current=1', 'current=2').replace('hwm=1', 'hwm=2'), 'current'],
  ['current_removed', descriptor(rings.retired).replace('current=2', 'current=1'), 'current'],
  ['hwm_below_version', d2.replace('hwm=2', 'hwm=1'), 'high_water_mark'],
].map(([id, descriptor, rule]) => ({ id, descriptor, rule: `descriptor:${rule}` }));
assert.equal(invalid_descriptors.find((v) => v.id === 'too_long').descriptor.length, 8193);

// Wraps: deterministic, so the blob is fixed by (keyring, current, DEK).
const wraps = [
  ['single', rings.single, DEK], ['single_other_dek', rings.single, DEK2], ['promoted', rings.promoted, DEK],
  ['other_name', rings.other_name, DEK], ['long_name', rings.long_name, DEK], ['largest', rings.largest, DEK],
].map(([id, r, dek]) => ({ id, descriptor: descriptor(r), dek_hex: hex(dek), blob_hex: hex(blob(r, r.current, dek)) }));
for (const w of wraps) assert.equal(w.blob_hex.length, 56 * 2);

// Reader rules over the `gap` keyring (k1, k3; current 3; hwm 3: version 2 was removed).
const gap = rings.gap;
const b1 = blob(gap, 1, DEK), b3 = blob(gap, 3, DEK);
const flip = (b, i) => { const c = Buffer.from(b); c[i] ^= 0x01; return c; };
const withVersion = (b, v) => Buffer.concat([b.subarray(0, 4), u32(v), b.subarray(8)]);
const unwrap = [
  ['current', b3, null, null, DEK],
  ['retained', b1, null, null, DEK],
  ['removed', withVersion(b1, 2), 'KeyVersionUnusable', 'GenerationRemoved'],
  ['above_hwm', withVersion(b3, 4), 'DecryptFailed', 'VersionAboveHighWaterMark'],
  ['above_hwm_max', withVersion(b3, 4294967295), 'DecryptFailed', 'VersionAboveHighWaterMark'],
  ['other_keyring', blob(rings.other_name, 1, DEK), 'DecryptFailed', 'KeyIdMismatch'],
  ['kid_flipped', flip(b3, 8), 'DecryptFailed', 'KeyIdMismatch'],
  ['wrapped_flipped', flip(b3, 55), 'DecryptFailed', null],
  ['icv_flipped', flip(b3, 16), 'DecryptFailed', null],
  ['body_of_other_generation', Buffer.concat([b1.subarray(0, 16), b3.subarray(16)]), 'DecryptFailed', null],
  ['magic_flipped', flip(b3, 0), 'DecryptFailed', null],
  ['old_magic', Buffer.concat([Buffer.from('akr1'), b3.subarray(4)]), 'DecryptFailed', null],
  ['version_zero', withVersion(b3, 0), 'DecryptFailed', null],
  ['truncated', b3.subarray(0, 55), 'DecryptFailed', null],
  ['extended', Buffer.concat([b3, Buffer.from([0])]), 'DecryptFailed', null],
  ['empty', Buffer.alloc(0), 'DecryptFailed', null],
  ['raw_dek', DEK, 'DecryptFailed', null],
].map(([id, b, kind, code, dek]) => ({ id, descriptor: descriptor(gap), blob_hex: hex(b), ...(dek ? { dek_hex: hex(dek) } : { kind, code }) }));

// Full envelopes: the purposes match tests/vectors.rs.
const envelopes = [];
const env = (id, r, { purpose, fields, plaintext, block = 0, slot = blob(r, r.current, DEK), kekId = r.name, readAad, expect = null }) => {
  const userAad = aad(purpose, fields);
  envelopes.push({
    id, descriptor: descriptor(r), purpose, fields, read_fields: readAad ?? fields, padding_block: block,
    aad_hex: hex(userAad), dek_hex: hex(DEK), nonce_hex: hex(NONCE), plaintext_hex: hex(plaintext),
    envelope_b64: envelope({ kekId, wrapped: slot, dek: DEK, nonce: NONCE, plaintext, userAad, block }).toString('base64'),
    expect,
  });
};
const session = { purpose: 'vector-session', fields: [['session_id', 'record-1']], plaintext: Buffer.from('a token') };
const email = {
  purpose: 'vector-email', block: 256, plaintext: Buffer.from('someone@example.com'),
  fields: [['row', '7'], ['batch_id', 'b-1'], ['account_id', '18446744073709551615']],
};
env('session', rings.single, session);
env('session_promoted', rings.promoted, session);
env('email_padded', rings.single, email);
env('wrong_field_value', rings.single, { ...session, readAad: [['session_id', 'record-2']], expect: { kind: 'DecryptFailed', code: null } });
env('other_kek_id', rings.single, { ...session, kekId: 'other', expect: { kind: 'DecryptFailed', code: null } });
env('above_hwm', rings.single, { ...session, slot: withVersion(blob(rings.single, 1, DEK), 2), expect: { kind: 'DecryptFailed', code: 'VersionAboveHighWaterMark' } });
env('removed', rings.retired, { ...session, slot: withVersion(blob(rings.retired, 2, DEK), 1), expect: { kind: 'KeyVersionUnusable', code: 'GenerationRemoved' } });
env('other_keyring_slot', rings.single, { ...session, slot: blob(rings.other_name, 1, DEK), expect: { kind: 'DecryptFailed', code: 'KeyIdMismatch' } });
env('swapped_slot', rings.single, { ...session, slot: blob(rings.single, 1, DEK2), expect: { kind: 'DecryptFailed', code: null } });

const vectors = {
  format: 'kekring v1 (docs/kekring-format.md); generated by scripts/kekring-vectors.mjs',
  labels: { descriptor_prefix: PREFIX, blob_magic_hex: hex(MAGIC), salt: SALT.toString(), kek: KEK_LABEL.toString(), kid: KID_LABEL.toString(), fp: FP_LABEL.toString() },
  derivation, descriptors, invalid_descriptors, wraps, unwrap, envelopes,
};

// The AAD encoder must reproduce upstream's valid vectors.
const upstream = JSON.parse(readFileSync(AAD_VECTORS, 'utf8'));
for (const v of upstream.valid) assert.equal(hex(aad(v.purpose, v.fields)), v.aad, `aad_v1.json ${v.name}`);

const text = `${JSON.stringify(vectors, null, 2)}\n`;
if (process.argv.includes('--check')) {
  assert.equal(readFileSync(OUT, 'utf8'), text, 'tests/vectors/kekring-v1.json is stale: rerun scripts/kekring-vectors.mjs');
  console.log('kekring-v1.json reproduced byte-exactly; aad_v1.json valid vectors reproduced');
} else {
  writeFileSync(OUT, text);
  console.log(`wrote ${OUT}`);
}
