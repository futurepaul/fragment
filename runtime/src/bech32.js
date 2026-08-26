// GENERATED from runtime/ts — run scripts/build-runtime after editing sources.
import { bech32 } from "@scure/base";
function npubFromHex(hex) {
  const bytes = new Uint8Array(hex.length / 2);
  for (let i = 0; i < bytes.length; i++) bytes[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return bech32.encode("npub", bech32.toWords(bytes));
}
function hexFromNpub(npub) {
  const { words } = bech32.decode(npub, 90);
  return [...bech32.fromWords(words)].map((b) => b.toString(16).padStart(2, "0")).join("");
}
export {
  hexFromNpub,
  npubFromHex
};
