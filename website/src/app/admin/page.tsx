import Link from "next/link";
import { redirect } from "next/navigation";
import { getCurrentUser } from "@/lib/auth";
import { prisma } from "@/lib/prisma";
import { AdminClient } from "./AdminClient";

export const dynamic = "force-dynamic";

export default async function AdminPage() {
  const user = await getCurrentUser();
  if (!user) redirect("/login?return_to=/admin");
  if (user.role !== "admin") {
    return (
      <div className="mx-auto max-w-2xl px-6 py-20 text-center">
        <h1 className="text-2xl font-semibold text-ink-50">Admin only</h1>
        <p className="mt-2 text-sm text-ink-400">
          You are signed in as <code className="font-mono text-ink-200">@{user.login}</code>{" "}
          (role: <code className="font-mono">{user.role}</code>). Ask a maintainer to promote your
          account.
        </p>
        <Link href="/" className="btn-ghost mt-6">
          Back to home
        </Link>
      </div>
    );
  }

  const [pending, recent, recentTweets, pkgCount, userCount] = await Promise.all([
    prisma.submission.findMany({
      where: { status: "pending" },
      orderBy: { createdAt: "asc" },
      include: { submitter: { select: { login: true, avatarUrl: true } } },
    }),
    prisma.submission.findMany({
      orderBy: { createdAt: "desc" },
      take: 20,
      include: { submitter: { select: { login: true, avatarUrl: true } } },
    }),
    prisma.tweet.findMany({ orderBy: { publishedAt: "desc" }, take: 20 }),
    prisma.package.count(),
    prisma.user.count(),
  ]);

  return (
    <AdminClient
      pending={pending.map((s) => ({
        id: s.id,
        name: s.name,
        repository: s.repository,
        ref: s.ref,
        version: s.version,
        description: s.description,
        license: s.license,
        authors: s.authors,
        createdAt: s.createdAt.toISOString(),
        submitter: s.submitter,
      }))}
      recent={recent.map((s) => ({
        id: s.id,
        name: s.name,
        status: s.status,
        createdAt: s.createdAt.toISOString(),
        decidedAt: s.decidedAt?.toISOString() ?? null,
        rejectionReason: s.rejectionReason,
        submitter: s.submitter,
      }))}
      tweets={recentTweets.map((t) => ({
        id: t.id,
        slug: t.slug,
        title: t.title,
        body: t.body,
        publishedAt: t.publishedAt.toISOString(),
      }))}
      stats={{ packages: pkgCount, users: userCount }}
    />
  );
}
