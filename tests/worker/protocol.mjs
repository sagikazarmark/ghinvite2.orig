import assert from 'node:assert/strict';

// Read-only observer for the pinned Restate protocol. Frames are never synthesized.
// https://github.com/restatedev/sdk-shared-core/blob/v7.0.3/service-protocol/dev/restate/service/protocol.proto
export function frames(bytes) {
  const result = [];
  for (let offset = 0; offset < bytes.length;) {
    assert.ok(offset + 8 <= bytes.length, 'complete protocol header');
    const end = offset + 8 + bytes.readUInt32BE(offset + 4);
    assert.ok(end <= bytes.length, 'complete protocol frame');
    result.push({ type: bytes.readUInt16BE(offset), payload: bytes.subarray(offset + 8, end), end });
    offset = end;
  }
  return result;
}

export function fields(bytes) {
  const result = new Map();
  let offset = 0;
  const varint = () => {
    let value = 0n, shift = 0n;
    for (;;) {
      assert.ok(offset < bytes.length && shift < 70n, 'bounded protobuf varint');
      const byte = bytes[offset++]; value += BigInt(byte & 127) << shift;
      if (!(byte & 128)) return Number(value);
      shift += 7n;
    }
  };
  while (offset < bytes.length) {
    const tag = varint(), field = Math.floor(tag / 8), wire = tag & 7;
    let value;
    if (wire === 0) value = varint();
    else if (wire === 2) { const length = varint(); value = bytes.subarray(offset, offset + length); offset += length; }
    else if (wire === 1) { value = bytes.subarray(offset, offset + 8); offset += 8; }
    else if (wire === 5) { value = bytes.subarray(offset, offset + 4); offset += 4; }
    else assert.fail(`unsupported protobuf wire ${wire}`);
    result.set(field, value);
  }
  return result;
}
