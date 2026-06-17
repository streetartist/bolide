import { NextResponse } from "next/server";
import { prisma } from "@/lib/prisma";

export const dynamic = "force-dynamic";

export async function GET() {
  const items = await prisma.tweet.findMany({
    orderBy: { publishedAt: "desc" },
    include: { author: { select: { login: true, avatarUrl: true } } },
  });
  return NextResponse.json({
    items: items.map((t) => ({
      id: t.id,
      slug: t.slug,
      title: t.title,
      body: t.body,
      publishedAt: t.publishedAt,
      author: { login: t.author.login, avatarUrl: t.author.avatarUrl },
    })),
  });
}
