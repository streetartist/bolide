import { z } from "zod";
import { prisma } from "@/lib/prisma";
import { requireAdmin } from "@/lib/auth";
import { handleApiError, jsonOk } from "@/lib/http";

export const dynamic = "force-dynamic";

const SlugRe = /^[a-z0-9][a-z0-9-]*$/;

const TweetBody = z.object({
  slug: z.string().min(2).max(80).regex(SlugRe, "slug must be lowercase alnum/hyphen"),
  title: z.string().min(1).max(140),
  body: z.string().min(1).max(20000),
});

export async function GET() {
  try {
    await requireAdmin();
    const items = await prisma.tweet.findMany({ orderBy: { publishedAt: "desc" } });
    return jsonOk({ items });
  } catch (e) {
    return handleApiError(e);
  }
}

export async function POST(req: Request) {
  try {
    const admin = await requireAdmin();
    const body = TweetBody.parse(await req.json());
    const created = await prisma.tweet.create({
      data: { ...body, authorId: admin.id },
    });
    return jsonOk({ ok: true, tweet: created });
  } catch (e) {
    return handleApiError(e);
  }
}
