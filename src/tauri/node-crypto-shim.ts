// The preload imports randomUUID from node:crypto; the webview has the Web Crypto one.
export const randomUUID = () => globalThis.crypto.randomUUID();
