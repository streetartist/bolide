import { z } from "zod";
import { prisma } from "@/lib/prisma";
import { requireUser } from "@/lib/auth";
import { handleApiError, jsonOk } from "@/lib/http";
import { parseRepoUrl, verifyRepoHasManifest } from "@/lib/github";

export const dynamic = "force-dynamic";

const SubmitBody = z.object({
  name: z
    .string()
    .min(2)
    .max(64)
    .regex(/^[a-z0-9][a-z0-9_-]*$/i, "package name must be alphanumeric / underscore / hyphen"),
  repository: z.string().url(),
  ref: z.string().min(1).max(128).default("main"),
  version: z.string().min(1).max(64),
  description: z.string().max(500).optional().nullable(),
  homepage: z.string().url().optional().nullable(),
  license: z.string().max(64).optional().nullable(),
  authors: z.string().max(200).optional().nullable(),
});

export async function POST(req: Request) {
  try {
    const user = await requireUser();
    const body = SubmitBody.parse(await req.json());

    // Parse + verify the repo URL up front. We do this synchronously so the
    // submitter sees an immediate error for an invalid URL or a missing
    // bolide.toml, rather than discovering it during admin review.
    let parsed;
    try {
      parsed = parseRepoUrl(body.repository);
    } catch (e) {
      return jsonOk({ ok: false, error: (e as Error).message }, { status: 400 });
    }
    try {
      await verifyRepoHasManifest(parsed, body.ref);
    } catch (e) {
      return jsonOk({ ok: false, error: (e as Error).message }, { status: 400 });
    }

    // Reject if a package with the same name is already approved — submitter
    // must pick a different name or contact admins to update.
    const existing = await prisma.package.findUnique({ where: { name: body.name } });
    if (existing) {
      return jsonOk(
        { ok: false, error: `Package name '${body.name}' is already registered` },
        { status: 409 },
      );
    }

    const submission = await prisma.submission.create({
      data: {
        name: body.name,
        repository: body.repository,
        ref: body.ref,
        version: body.version,
        description: body.description ?? null,
        homepage: body.homepage ?? null,
        license: body.license ?? null,
        authors: body.authors ?? null,
        submitterId: user.id,
        status: "pending",
      },
    });

    return jsonOk({ ok: true, submissionId: submission.id });
  } catch (e) {
    return handleApiError(e);
  }
}
