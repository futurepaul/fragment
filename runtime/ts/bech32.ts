// bech32 (BIP-173) for npub <-> hex, on @scure/base — the audited noble
// ecosystem the runtime already trusts for schnorr. Checksum and padding
// subtleties are exactly what a hand-roll gets silently wrong.
import { bech32 } from "@scure/base";

export function npubFromHex(hex: string): string {
  const bytes = new Uint8Array(hex.length / 2);
  for (let i = 0; i < bytes.length; i++) bytes[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return bech32.encode("npub", bech32.toWords(bytes));
}

export function hexFromNpub(npub: string): string {
  const { words } = bech32.decode(npub as any, 90);
  return [...bech32.fromWords(words)].map((b: number) => b.toString(16).padStart(2, "0")).join("");
}
