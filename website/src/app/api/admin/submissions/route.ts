import { NextResponse } from "next/server";
import { prisma } from "@/lib/prisma";
import { requireAdmin } from "@/lib/auth";
import { handleApiError, jsonOk } from "@/lib/http";

export const dynamic = "force-dynamic";

export async function GET() {
  try {
    await requireAdmin();
    const items = await prisma.submission.findMany({
      orderBy: { createdAt: "desc" },
      include: { submitter: { select: { login: true, avatarUrl: true } } },
    });
    return NextResponse.json({ items });
  } catch (e) {
    return handleApiError(e);
  }
}

export async function POST() {
  // No-op: submissions are created via /api/packages/submit. Admin endpoints
  // are approve/reject (see [id]/route.ts).
  return jsonOk({ ok: true });
}
