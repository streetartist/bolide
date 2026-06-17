import { prisma } from "@/lib/prisma";

export const dynamic = "force-dynamic";

function renderBody(body: string) {
  // Minimal markdown: paragraphs by double newline, code by backticks.
  return body.split(/\n{2,}/).map((para, i) => {
    if (/^```/.test(para)) {
      const inner = para.replace(/^```[a-z]*\n?/, "").replace(/```$/, "");
      return (
        <pre key={i} className="code-block my-4 overflow-x-auto">
          {inner}
        </pre>
      );
    }
    return (
      <p key={i} className="my-3 text-ink-300">
        {para.split(/`([^`]+)`/g).map((seg, j) =>
          j % 2 === 1 ? (
            <code key={j} className="rounded bg-ink-900 px-1.5 py-0.5 font-mono text-ink-100">
              {seg}
            </code>
          ) : (
            <span key={j}>{seg}</span>
          ),
        )}
      </p>
    );
  });
}

export default async function TweetsPage() {
  const items = await prisma.tweet.findMany({
    orderBy: { publishedAt: "desc" },
    include: { author: { select: { login: true, avatarUrl: true } } },
  });

  return (
    <div className="mx-auto max-w-3xl px-6 py-12">
      <h1 className="text-3xl font-semibold text-ink-50">Tweets</h1>
      <p className="mt-2 text-ink-400">Short posts from the Bolide team.</p>

      <div className="mt-8 space-y-6">
        {items.length === 0 && <p className="text-sm text-ink-400">Nothing posted yet.</p>}
        {items.map((t) => (
          <article key={t.id} id={t.slug} className="card">
            <header className="flex items-center justify-between gap-3">
              <h2 className="text-lg font-semibold text-ink-50">{t.title}</h2>
              <time className="text-xs text-ink-400">
                {new Date(t.publishedAt).toLocaleString()}
              </time>
            </header>
            <div className="mt-3 text-sm">{renderBody(t.body)}</div>
            <footer className="mt-4 flex items-center gap-2 text-xs text-ink-400">
              {t.author.avatarUrl ? (
                // eslint-disable-next-line @next/next/no-img-element
                <img
                  src={t.author.avatarUrl}
                  alt={t.author.login}
                  className="h-5 w-5 rounded-full border border-ink-800"
                />
              ) : null}
              <span>@{t.author.login}</span>
            </footer>
          </article>
        ))}
      </div>
    </div>
  );
}
