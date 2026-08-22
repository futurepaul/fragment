// bech32 (BIP-173) encode/decode — enough for npub <-> hex.
const CHARSET = "qpzry9x8gf2tvdw0s3jn54khce6mua7l";
const REV = Object.fromEntries([...CHARSET].map((c, i) => [c, i]));

function polymod(values) {
  const GEN = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];
  let chk = 1;
  for (const v of values) {
    const top = chk >> 25;
    chk = ((chk & 0x1ffffff) << 5) ^ v;
    for (let i = 0; i < 5; i++) if ((top >> i) & 1) chk ^= GEN[i];
  }
  return chk;
}

function hrpExpand(hrp) {
  return [...hrp].map((c) => c.charCodeAt(0) >> 5).concat([0], [...hrp].map((c) => c.charCodeAt(0) & 31));
}

function convertBits(data, from, to, pad) {
  let acc = 0, bits = 0;
  const out = [];
  const maxv = (1 << to) - 1;
  for (const v of data) {
    if (v < 0 || v >> from !== 0) throw new Error("bech32: bad value");
    acc = (acc << from) | v;
    bits += from;
    while (bits >= to) {
      bits -= to;
      out.push((acc >> bits) & maxv);
    }
  }
  if (pad) {
    if (bits > 0) out.push((acc << (to - bits)) & maxv);
  } else if (bits >= from || ((acc << (to - bits)) & maxv)) {
    throw new Error("bech32: bad padding");
  }
  return out;
}

export function npubFromHex(hex) {
  const bytes = hex.match(/.{2}/g).map((b) => parseInt(b, 16));
  const words = convertBits(bytes, 8, 5, true);
  const hrp = "npub";
  const pm = polymod(hrpExpand(hrp).concat(words, [0, 0, 0, 0, 0, 0])) ^ 1;
  const checksum = [];
  for (let i = 0; i < 6; i++) checksum.push((pm >> (5 * (5 - i))) & 31);
  return hrp + "1" + words.concat(checksum).map((w) => CHARSET[w]).join("");
}

export function hexFromNpub(npub) {
  if (typeof npub !== "string" || !npub.startsWith("npub1")) throw new Error("bad npub");
  const hrp = npub.slice(0, npub.lastIndexOf("1"));
  const chars = npub.slice(npub.lastIndexOf("1") + 1);
  if (chars.length < 7) throw new Error("bad npub");
  const words = [...chars].map((c) => {
    const v = REV[c];
    if (v === undefined) throw new Error("bad npub char");
    return v;
  });
  if (polymod(hrpExpand(hrp).concat(words)) !== 1) throw new Error("bad npub checksum");
  const data = words.slice(0, -6);
  const bytes = convertBits(data, 5, 8, false);
  return bytes.map((b) => b.toString(16).padStart(2, "0")).join("");
}
