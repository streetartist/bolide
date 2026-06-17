import { z } from "zod";
import { prisma } from "@/lib/prisma";
import { requireAdmin } from "@/lib/auth";
import { handleApiError, jsonOk } from "@/lib/http";
import { parseRepoUrl, tarballUrl, fetchTarballChecksum, fetchRepoMeta } from "@/lib/github";

export const dynamic = "force-dynamic";

const DecideBody = z.object({
  action: z.enum(["approve", "reject"]),
  reason: z.string().max(500).optional(),
});

export async function POST(req: Request, ctx: { params: { id: string } }) {
  try {
    const admin = await requireAdmin();
    const body = DecideBody.parse(await req.json());

    const sub = await prisma.submission.findUnique({ where: { id: ctx.params.id } });
    if (!sub) return jsonOk({ ok: false, error: "submission not found" }, { status: 404 });
    if (sub.status !== "pending") {
      return jsonOk({ ok: false, error: `submission already ${sub.status}` }, { status: 409 });
    }

    if (body.action === "reject") {
      const updated = await prisma.submission.update({
        where: { id: sub.id },
        data: {
          status: "rejected",
          rejectionReason: body.reason ?? null,
          decidedAt: new Date(),
        },
      });
      return jsonOk({ ok: true, submission: updated });
    }

    // Approve flow:
    //  1. parse repo URL → owner/repo
    //  2. compute tarball sha256 (best-effort, may fail behind firewalls)
    //  3. best-effort repo metadata enrichment
    //  4. create Package + PackageVersion atomically
    const parsed = parseRepoUrl(sub.repository);
    const downloadUrl = tarballUrl(parsed, sub.ref);

    let checksum: string;
    try {
      checksum = await fetchTarballChecksum(parsed, sub.ref);
    } catch (e) {
      return jsonOk(
        { ok: false, error: `Failed to fetch tarball for checksum: ${(e as Error).message}` },
        { status: 502 },
      );
    }

    let license: string | null = sub.license;
    let description: string | null = sub.description;
    let homepage: string | null = sub.homepage;
    try {
      const meta = await fetchRepoMeta(parsed);
      if (!description && meta.description) description = meta.description;
      if (!license && meta.license?.spdx_id) license = meta.license.spdx_id;
      if (!homepage) homepage = meta.html_url;
    } catch {
      // Non-fatal: we already have the submitter's data.
    }

    const result = await prisma.$transaction(async (tx) => {
      const pkg = await tx.package.create({
        data: {
          name: sub.name,
          description,
          homepage,
          repository: sub.repository,
          owner: parsed.owner,
          repo: parsed.repo,
          license,
          authors: sub.authors,
          reviewerId: admin.id,
          status: "approved",
        },
      });
      const version = await tx.packageVersion.create({
        data: {
          packageId: pkg.id,
          version: sub.version,
          ref: sub.ref,
          downloadUrl,
          checksum,
        },
      });
      const updatedSub = await tx.submission.update({
        where: { id: sub.id },
        data: { status: "approved", decidedAt: new Date(), packageId: pkg.id },
      });
      return { pkg, version, updatedSub };
    });

    return jsonOk({ ok: true, package: result.pkg, version: result.version });
  } catch (e) {
    return handleApiError(e);
  }
}
