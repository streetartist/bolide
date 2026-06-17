import Link from "next/link";
import { prisma } from "@/lib/prisma";

export const dynamic = "force-dynamic";

export default async function PackagesPage({
  searchParams,
}: {
  searchParams: { q?: string };
}) {
  const q = (searchParams.q ?? "").trim();
  const where: Record<string, unknown> = { status: "approved" };
  if (q) where.name = { contains: q };

  const items = await prisma.package.findMany({
    where,
    orderBy: [{ updatedAt: "desc" }],
    take: 100,
    include: {
      versions: { where: { yank: false }, orderBy: { createdAt: "desc" }, take: 1 },
    },
  });

  return (
    <div className="mx-auto max-w-5xl px-6 py-12">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div>
          <h1 className="text-3xl font-semibold text-ink-50">Packages</h1>
          <p className="mt-2 text-ink-400">
            {items.length} approved package{items.length === 1 ? "" : "s"}. Submissions are reviewed
            by the Bolide team before they appear here.
          </p>
        </div>
        <Link href="/submit" className="btn-primary">
          Submit a package
        </Link>
      </div>

      <form className="mt-6" action="/packages">
        <input
          type="search"
          name="q"
          defaultValue={q}
          placeholder="Search by package name…"
          className="input"
        />
      </form>

      <div className="mt-6 divide-y divide-ink-800 rounded-xl border border-ink-800 bg-ink-900/40">
        {items.length === 0 ? (
          <div className="p-10 text-center text-sm text-ink-400">
            No packages match. <Link href="/submit" className="text-accent-400 hover:text-accent-500">Submit one?</Link>
          </div>
        ) : (
          items.map((p) => (
            <Link
              key={p.id}
              href={`/packages/${p.name}`}
              className="flex items-center justify-between gap-4 px-5 py-4 transition hover:bg-ink-900"
            >
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <span className="font-mono text-sm text-ink-50">{p.name}</span>
                  {p.license && (
                    <span className="rounded-full border border-ink-800 px-2 py-0.5 text-[10px] uppercase tracking-wider text-ink-400">
                      {p.license}
                    </span>
                  )}
                </div>
                {p.description && (
                  <p className="mt-1 line-clamp-1 text-sm text-ink-400">{p.description}</p>
                )}
              </div>
              <div className="shrink-0 text-right text-xs text-ink-400">
                <div className="font-mono text-accent-400">{p.versions[0]?.version ?? "—"}</div>
                <div>{new Date(p.updatedAt).toLocaleDateString()}</div>
              </div>
            </Link>
          ))
        )}
      </div>
    </div>
  );
}
