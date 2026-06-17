// bolide-pkg compatible index route.
// bolide-pkg constructs: `${registry}/{prefix}/{name}.json` where prefix is
// `<a-b>/<c>` (or `<a>` for short names). We just take the file stem as the
// canonical package name and serve the IndexEntry JSON.

import { NextResponse } from "next/server";
import { buildIndexEntry } from "@/lib/registry-proto";

export const dynamic = "force-dynamic";

export async function GET(_req: Request, ctx: { params: { path: string[] } }) {
  const segs = ctx.params.path || [];
  const last = segs[segs.length - 1] || "";
  const name = last.endsWith(".json") ? last.slice(0, -5) : last;
  if (!name) return NextResponse.json({ error: "missing package name" }, { status: 400 });

  const entry = await buildIndexEntry(name);
  if (!entry) {
    // Mirror bolide-pkg's behavior: a 404 here causes `bolide add` to surface
    // "Version not found" upstream. Avoid leaking existence of pending/yanked
    // packages.
    return NextResponse.json({ error: "not found" }, { status: 404 });
  }
  return NextResponse.json(entry, {
    headers: {
      "Cache-Control": "public, max-age=60, s-maxage=300",
      "Content-Type": "application/json; charset=utf-8",
    },
  });
}
