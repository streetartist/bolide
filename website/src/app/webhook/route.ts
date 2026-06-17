// GitHub webhook receiver.
// Verifies X-Hub-Signature-256, persists the raw payload, marks processed.
// v1: log + persist only; future work can fan out to release handlers.

import { NextResponse } from "next/server";
import { prisma } from "@/lib/prisma";
import { verifyGithubSignature } from "@/lib/webhook";

export const dynamic = "force-dynamic";

export async function POST(req: Request) {
  const raw = await req.text();
  const sig = req.headers.get("x-hub-signature-256");
  const event = req.headers.get("x-github-event") ?? "unknown";
  const delivery = req.headers.get("x-github-delivery") ?? undefined;
  const secret = process.env.GITHUB_WEBHOOK_SECRET ?? "";

  if (!secret) {
    return NextResponse.json({ error: "webhook not configured" }, { status: 503 });
  }
  if (!verifyGithubSignature(raw, sig, secret)) {
    return NextResponse.json({ error: "bad signature" }, { status: 401 });
  }

  const record = await prisma.webhookEvent.create({
    data: {
      source: "github",
      event,
      delivery,
      signature: sig,
      payload: raw,
    },
  });

  try {
    // eslint-disable-next-line no-console
    console.log(`[webhook] github event=${event} delivery=${delivery ?? "-"}`);
    await prisma.webhookEvent.update({
      where: { id: record.id },
      data: { processed: true },
    });
    return NextResponse.json({ ok: true });
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    await prisma.webhookEvent.update({
      where: { id: record.id },
      data: { processed: false, error: msg },
    });
    return NextResponse.json({ error: msg }, { status: 500 });
  }
}

export async function GET() {
  return NextResponse.json({ ok: true, info: "POST github webhook events here" });
}
