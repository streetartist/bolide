import { prisma } from "@/lib/prisma";
import { jsonError, jsonOk } from "@/lib/http";

export const dynamic = "force-dynamic";

export async function GET(_req: Request, ctx: { params: { name: string } }) {
  const pkg = await prisma.package.findUnique({
    where: { name: ctx.params.name },
    include: {
      versions: {
        where: { yank: false },
        orderBy: { createdAt: "desc" },
      },
    },
  });
  if (!pkg || pkg.status !== "approved") return jsonError(404, "package not found");
  return jsonOk({
    name: pkg.name,
    description: pkg.description,
    homepage: pkg.homepage,
    repository: pkg.repository,
    license: pkg.license,
    authors: pkg.authors,
    versions: pkg.versions.map((v) => ({
      version: v.version,
      ref: v.ref,
      checksum: v.checksum,
      downloadUrl: v.downloadUrl,
    })),
  });
}
