import { randomBytes, createHash } from 'node:crypto';

// Browser-side fixture memory only. Only SHA256 and generation enter Restate.
const capabilities = new Map();
export function capabilityFor(linkId) {
  if (!capabilities.has(linkId)) {
    const bytes = randomBytes(32);
    capabilities.set(linkId, {
      secret: bytes.toString('base64url'),
      sha256: createHash('sha256').update(bytes).digest('hex'),
      generation: 1,
    });
    bytes.fill(0);
  }
  return capabilities.get(linkId);
}
export function admitCapability(linkId) {
  const capability = capabilityFor(linkId);
  return { secret_sha256: capability.sha256, secret_generation: capability.generation, scope_evidence: [] };
}
