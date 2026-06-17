import { NextResponse } from "next/server";
import { HttpError } from "./auth";

// `jsonOk` is a stable boundary used by 7+ route handlers, so keep the name.
export function jsonOk<T>(data: T, init?: ResponseInit) {
  return NextResponse.json(data, init);
}

export function jsonError(status: number, message: string) {
  return NextResponse.json({ error: message }, { status });
}

export function handleApiError(e: unknown) {
  if (e instanceof HttpError) return jsonError(e.status, e.message);
  const msg = e instanceof Error ? e.message : String(e);
  // eslint-disable-next-line no-console
  console.error("[api]", msg);
  return jsonError(500, msg);
}
