import Link from "next/link";
import { prisma } from "@/lib/prisma";

const FEATURES = [
  { title: "JIT & AOT", body: "Cranelift-backed compilation, native performance, fast cold start." },
  { title: "First-class functions", body: "Closures, higher-order methods, generic functions with inference." },
  { title: "Async / await", body: "Lightweight coroutines, spawn all/select, thread pool primitives." },
  { title: "Bidirectional FFI", body: "Call C from Bolide, expose Bolide to C with `export fn` + `--header`." },
  { title: "Built-in package manager", body: "git, path, or registry sources — locked, reproducible, cache-friendly." },
  { title: "Web & GUI std", body: "HTTP routing, sessions, AOT single-file deploy, and a retained-mode GUI toolkit." },
];

const SNIPPET = `// hello.bl — print a greeting, build a list, map it.
fn double(x: int) -> int { return x * 2; }

let nums: list<int> = [1, 2, 3, 4, 5];
let out: list<str> = nums.map(double).map(str);

for s in out {
    print("hello, " + s);
}
`;

export default async function HomePage() {
  const packageCount = await prisma.package.count({ where: { status: "approved" } });
  const latestTweets = await prisma.tweet.findMany({
    orderBy: { publishedAt: "desc" },
    take: 3,
    include: { author: { select: { login: true, avatarUrl: true } } },
  });

  return (
    <div className="mx-auto max-w-6xl px-6 py-16">
      <section className="grid gap-10 md:grid-cols-[1.2fr,1fr] md:items-center">
        <div>
          <p className="text-xs uppercase tracking-[0.2em] text-accent-400">v0.12 · MIT</p>
          <h1 className="mt-3 text-4xl font-semibold leading-tight text-ink-50 md:text-6xl">
            A modern language
            <br />
            that compiles to <span className="text-accent-400">native code</span>.
          </h1>
          <p className="mt-6 max-w-prose text-lg text-ink-400">
            Bolide is a JIT/AOT compiled language with clean syntax, first-class functions, async/await,
            rich types, and a built-in package manager. {packageCount} curated packages in the
            official index.
          </p>
          <div className="mt-8 flex flex-wrap gap-3">
            <Link href="/download" className="btn-primary">
              Download v0.12
            </Link>
            <Link href="/packages" className="btn-ghost">
              Browse packages
            </Link>
            <Link href="https://github.com/bolide-lang/bolide" className="btn-ghost">
              Source on GitHub
            </Link>
          </div>
        </div>
        <pre className="code-block overflow-x-auto md:text-[13px]">{SNIPPET}</pre>
      </section>

      <section className="mt-20 grid gap-4 md:grid-cols-3">
        {FEATURES.map((f) => (
          <div key={f.title} className="card">
            <h3 className="text-base font-semibold text-ink-50">{f.title}</h3>
            <p className="mt-2 text-sm text-ink-400">{f.body}</p>
          </div>
        ))}
      </section>

      {latestTweets.length > 0 && (
        <section className="mt-20">
          <div className="mb-6 flex items-baseline justify-between">
            <h2 className="text-2xl font-semibold text-ink-50">Latest from the team</h2>
            <Link href="/tweets" className="text-sm text-accent-400 hover:text-accent-500">
              View all →
            </Link>
          </div>
          <div className="grid gap-4 md:grid-cols-3">
            {latestTweets.map((t) => (
              <Link
                key={t.id}
                href={`/tweets#${t.slug}`}
                className="card transition hover:border-accent-500/60"
              >
                <h3 className="text-base font-semibold text-ink-50">{t.title}</h3>
                <p className="mt-2 line-clamp-3 text-sm text-ink-400">{t.body}</p>
                <p className="mt-3 text-xs text-ink-600">
                  {new Date(t.publishedAt).toLocaleDateString()} · @{t.author.login}
                </p>
              </Link>
            ))}
          </div>
        </section>
      )}
    </div>
  );
}
