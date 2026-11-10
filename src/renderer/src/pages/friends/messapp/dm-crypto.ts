import {
  decryptText,
  deriveSharedKey,
  encryptText,
  importPrivateKey,
  type AesEnvelope,
} from "./crypto.js";

/** MESSAPP stamps each DM with both public keys, so old messages stay readable after a key change. */
export interface DmEnvelope extends AesEnvelope {
  spub?: JsonWebKey;
  tpub?: JsonWebKey;
}

export interface DmKey {
  privateKey: CryptoKey;
  publicJwk: JsonWebKey;
}

/** The current key plus the keys of earlier rotations, which still open old DMs. */
export interface DmKeyring extends DmKey {
  legacy: DmKey[];
}

const toKey = async (privateJwk: JsonWebKey): Promise<DmKey> => ({
  privateKey: await importPrivateKey(privateJwk),
  publicJwk: {
    kty: privateJwk.kty,
    crv: privateJwk.crv,
    x: privateJwk.x,
    y: privateJwk.y,
    ext: true,
  },
});

export const createKeyring = async (
  privateJwk: JsonWebKey,
  legacyJwks: JsonWebKey[] = []
): Promise<DmKeyring> => {
  // A legacy key that fails to import is skipped, as in MESSAPP.
  const legacy = await Promise.all(
    legacyJwks.map((jwk) => toKey(jwk).catch(() => null))
  );

  return {
    ...(await toKey(privateJwk)),
    legacy: legacy.filter((key): key is DmKey => key !== null),
  };
};

export const encryptDm = async (
  keyring: DmKeyring,
  peerPublicJwk: JsonWebKey,
  text: string
) => {
  const shared = await deriveSharedKey(keyring.privateKey, peerPublicJwk);
  const envelope: DmEnvelope = {
    ...(await encryptText(shared, text)),
    spub: keyring.publicJwk,
    tpub: { ...peerPublicJwk, ext: true },
  };

  return JSON.stringify(envelope);
};

const parseEnvelope = (content: string): DmEnvelope | null => {
  if (!content.startsWith("{")) return null;

  try {
    const parsed = JSON.parse(content);
    return parsed?.iv && parsed?.ciphertext ? parsed : null;
  } catch {
    return null;
  }
};

/**
 * Key pairs to try, in shaiyon-crypto's order (MESSAPP's): first the public keys the message
 * carries (historical_keys), then the DM's keys (build_shared_keys): own + peer, own + own
 * public (notes to self), and per legacy key: legacy + peer, legacy + its public, own + legacy public.
 */
const candidates = (
  keyring: DmKeyring,
  envelope: DmEnvelope,
  sentByMe: boolean,
  peer: JsonWebKey | null
): [CryptoKey, JsonWebKey][] => {
  const pairs: [CryptoKey, JsonWebKey][] = [];
  if (envelope.spub && envelope.tpub) {
    const theirs = sentByMe ? envelope.tpub : envelope.spub;
    for (const key of [keyring, ...keyring.legacy]) {
      pairs.push([key.privateKey, theirs]);
    }
  }
  if (peer) pairs.push([keyring.privateKey, peer]);
  pairs.push([keyring.privateKey, keyring.publicJwk]);
  for (const old of keyring.legacy) {
    if (peer) pairs.push([old.privateKey, peer]);
    pairs.push([old.privateKey, old.publicJwk]);
    pairs.push([keyring.privateKey, old.publicJwk]);
  }
  return pairs;
};

/** Plain text, or null when no key we hold can open it (e.g. sealed with a lost legacy key). */
export const decryptDm = async (
  keyring: DmKeyring,
  content: string,
  sentByMe: boolean,
  peerPublicJwk: JsonWebKey | null
): Promise<string | null> => {
  const envelope = parseEnvelope(content);
  if (!envelope) return content;

  for (const [privateKey, publicJwk] of candidates(
    keyring,
    envelope,
    sentByMe,
    peerPublicJwk
  )) {
    try {
      const shared = await deriveSharedKey(privateKey, publicJwk);
      const text = await decryptText(shared, envelope);
      if (text !== null) return text;
    } catch {
      // A malformed public key; try the next pair.
    }
  }
  return null;
};
