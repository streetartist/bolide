import { z } from "zod";
import { prisma } from "@/lib/prisma";
import { requireAdmin } from "@/lib/auth";
import { handleApiError, jsonOk } from "@/lib/http";

export const dynamic = "force-dynamic";

const Patch = z.object({
  slug: z.string().min(2).max(80).optional(),
  title: z.string().min(1).max(140).optional(),
  body: z.string().min(1).max(20000).optional(),
});

export async function PATCH(req: Request, ctx: { params: { id: string } }) {
  try {
    await requireAdmin();
    const body = Patch.parse(await req.json());
    const updated = await prisma.tweet.update({ where: { id: ctx.params.id }, data: body });
    return jsonOk({ ok: true, tweet: updated });
  } catch (e) {
    return handleApiError(e);
  }
}

export async function DELETE(_req: Request, ctx: { params: { id: string } }) {
  try {
    await requireAdmin();
    await prisma.tweet.delete({ where: { id: ctx.params.id } });
    return jsonOk({ ok: true });
  } catch (e) {
    return handleApiError(e);
  }
}
