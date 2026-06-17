// GitHub webhook signature verification.
// GitHub sends `X-Hub-Signature-256: sha256=<hex>` where the hex is HMAC-SHA256
// of the raw body keyed by the shared webhook secret.

import crypto from "node:crypto";

export function verifyGithubSignature(
  rawBody: string,
  signature: string | null,
  secret: string,
): boolean {
  if (!signature) return false;
  const expected = crypto.createHmac("sha256", secret).update(rawBody).digest("hex");
  const provided = signature.startsWith("sha256=") ? signature.slice(7) : signature;
  if (provided.length !== expected.length) return false;
  return crypto.timingSafeEqual(Buffer.from(provided, "hex"), Buffer.from(expected, "hex"));
}
