import Link from "next/link";
import { notFound } from "next/navigation";
import { prisma } from "@/lib/prisma";
import { indexPathFor } from "@/lib/registry-proto";

export const dynamic = "force-dynamic";

export default async function PackageDetailPage({ params }: { params: { name: string } }) {
  const pkg = await prisma.package.findUnique({
    where: { name: params.name },
    include: {
      versions: { orderBy: { createdAt: "desc" } },
      reviewer: { select: { login: true } },
    },
  });
  if (!pkg || pkg.status !== "approved") notFound();

  const latest = pkg.versions.find((v) => !v.yank) ?? pkg.versions[0];
  const registry = process.env.SITE_ORIGIN || "";
  const install =
    latest != null
      ? `bolide add ${pkg.name}@${latest.version} --registry ${registry}`
      : `# no published versions yet`;

  return (
    <div className="mx-auto max-w-4xl px-6 py-12">
      <Link href="/packages" className="text-sm text-ink-400 hover:text-ink-50">
        ← All packages
      </Link>
      <header className="mt-3 flex flex-wrap items-start justify-between gap-4">
        <div className="min-w-0">
          <h1 className="font-mono text-3xl font-semibold text-ink-50">{pkg.name}</h1>
          {pkg.description && <p className="mt-2 max-w-2xl text-ink-400">{pkg.description}</p>}
          <div className="mt-3 flex flex-wrap items-center gap-2 text-xs text-ink-400">
            {pkg.license && (
              <span className="rounded-full border border-ink-800 px-2 py-0.5 uppercase tracking-wider">
                {pkg.license}
              </span>
            )}
            {pkg.authors && <span>by {pkg.authors}</span>}
            {pkg.reviewer && <span>· approved by @{pkg.reviewer.login}</span>}
            <a
              href={pkg.repository}
              className="text-accent-400 hover:text-accent-500"
              target="_blank"
              rel="noreferrer"
            >
              Repository ↗
            </a>
            {pkg.homepage && (
              <a
                href={pkg.homepage}
                className="text-accent-400 hover:text-accent-500"
                target="_blank"
                rel="noreferrer"
              >
                Homepage ↗
              </a>
            )}
          </div>
        </div>
        <Link href="/submit" className="btn-ghost text-xs">
          Update package
        </Link>
      </header>

      <section className="mt-8">
        <h2 className="text-sm font-semibold uppercase tracking-wider text-ink-400">Install</h2>
        <pre className="code-block mt-2 overflow-x-auto">{install}</pre>
        <p className="mt-2 text-xs text-ink-400">
          The official index URL is{" "}
          <code className="font-mono text-ink-200">
            {registry}
            {indexPathFor(pkg.name)}/{pkg.name}.json
          </code>
          . bolide-pkg uses this layout transparently.
        </p>
      </section>

      <section className="mt-8">
        <h2 className="text-sm font-semibold uppercase tracking-wider text-ink-400">Versions</h2>
        <div className="mt-2 divide-y divide-ink-800 rounded-xl border border-ink-800 bg-ink-900/40">
          {pkg.versions.map((v) => (
            <div key={v.id} className="flex items-center justify-between gap-3 px-5 py-3 text-sm">
              <div className="flex items-center gap-3">
                <span className="font-mono text-ink-50">{v.version}</span>
                <span className="text-xs text-ink-400">ref {v.ref}</span>
                {v.yank && (
                  <span className="rounded-full border border-red-700/40 bg-red-900/30 px-2 py-0.5 text-[10px] uppercase tracking-wider text-red-200">
                    yanked
                  </span>
                )}
              </div>
              <div className="font-mono text-xs text-ink-400">sha256:{v.checksum.slice(0, 16)}…</div>
            </div>
          ))}
        </div>
      </section>
    </div>
  );
}
