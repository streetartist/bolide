import { NextResponse } from "next/server";
import { prisma } from "@/lib/prisma";

export const dynamic = "force-dynamic";

export async function GET(req: Request) {
  const url = new URL(req.url);
  const q = url.searchParams.get("q")?.trim() ?? "";
  const limit = Math.min(parseInt(url.searchParams.get("limit") ?? "50", 10) || 50, 200);

  const where: Record<string, unknown> = { status: "approved" };
  if (q) where.name = { contains: q };

  const items = await prisma.package.findMany({
    where,
    orderBy: [{ updatedAt: "desc" }],
    take: limit,
    include: {
      versions: {
        where: { yank: false },
        orderBy: { createdAt: "desc" },
        take: 1,
      },
    },
  });

  return NextResponse.json({
    items: items.map((p) => ({
      name: p.name,
      description: p.description,
      repository: p.repository,
      latestVersion: p.versions[0]?.version ?? null,
      updatedAt: p.updatedAt,
    })),
  });
}
