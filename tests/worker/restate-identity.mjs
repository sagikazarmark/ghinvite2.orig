// Actual Ed25519 request signatures, generated per run; no external credentials.
import { generateKeyPairSync, sign } from 'node:crypto';

const alphabet = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
function base58(bytes) {
  let value = BigInt(`0x${bytes.toString('hex')}`);
  let encoded = '';
  while (value > 0n) { encoded = alphabet[Number(value % 58n)] + encoded; value /= 58n; }
  for (const byte of bytes) { if (byte !== 0) break; encoded = '1' + encoded; }
  return encoded;
}
export function identitySigner() {
  const { privateKey, publicKey } = generateKeyPairSync('ed25519');
  const raw = Buffer.from(publicKey.export({ format: 'jwk' }).x, 'base64url');
  const key = `publickeyv1_${base58(raw)}`;
  return {
    key,
    headers(path, overrides = {}) {
      const now = Math.floor(Date.now() / 1000);
      const header = { alg: 'EdDSA', typ: 'JWT', kid: key };
      const claims = { aud: path, iat: now, nbf: now - 60, exp: now + 60, ...overrides };
      const payload = [header, claims].map(value => Buffer.from(JSON.stringify(value)).toString('base64url')).join('.');
      const signature = sign(null, Buffer.from(payload), privateKey).toString('base64url');
      return { 'x-restate-signature-scheme': 'v1', 'x-restate-jwt-v1': `${payload}.${signature}` };
    },
  };
}
