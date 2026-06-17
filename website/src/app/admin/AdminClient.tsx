"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";

type Submission = {
  id: string;
  name: string;
  repository: string;
  ref: string;
  version: string;
  description: string | null;
  license: string | null;
  authors: string | null;
  createdAt: string;
  submitter: { login: string; avatarUrl: string | null };
};

type RecentSubmission = {
  id: string;
  name: string;
  status: string;
  createdAt: string;
  decidedAt: string | null;
  rejectionReason: string | null;
  submitter: { login: string; avatarUrl: string | null };
};

type Tweet = { id: string; slug: string; title: string; body: string; publishedAt: string };

type Props = {
  pending: Submission[];
  recent: RecentSubmission[];
  tweets: Tweet[];
  stats: { packages: number; users: number };
};

export function AdminClient(props: Props) {
  const router = useRouter();
  const [busyId, setBusyId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [tweetDraft, setTweetDraft] = useState({ slug: "", title: "", body: "" });
  const [tweetBusy, setTweetBusy] = useState(false);

  async function decide(id: string, action: "approve" | "reject", reason?: string) {
    setBusyId(id);
    setError(null);
    try {
      const res = await fetch(`/api/admin/submissions/${id}`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ action, reason }),
      });
      const data = await res.json();
      if (!res.ok || !data.ok) {
        setError(data.error || `HTTP ${res.status}`);
        return;
      }
      router.refresh();
    } finally {
      setBusyId(null);
    }
  }

  async function createTweet(e: React.FormEvent) {
    e.preventDefault();
    setTweetBusy(true);
    setError(null);
    try {
      const res = await fetch("/api/admin/tweets", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(tweetDraft),
      });
      const data = await res.json();
      if (!res.ok || !data.ok) {
        setError(data.error || `HTTP ${res.status}`);
        return;
      }
      setTweetDraft({ slug: "", title: "", body: "" });
      router.refresh();
    } finally {
      setTweetBusy(false);
    }
  }

  async function deleteTweet(id: string) {
    if (!confirm("Delete this tweet?")) return;
    setBusyId(id);
    const res = await fetch(`/api/admin/tweets/${id}`, { method: "DELETE" });
    if (!res.ok) {
      setError(`delete failed: ${res.status}`);
    } else {
      router.refresh();
    }
    setBusyId(null);
  }

  return (
    <div className="mx-auto max-w-6xl px-6 py-10">
      <header className="flex flex-wrap items-baseline justify-between gap-3">
        <h1 className="text-3xl font-semibold text-ink-50">Admin</h1>
        <p className="text-sm text-ink-400">
          {props.stats.packages} packages · {props.stats.users} users · {props.pending.length} pending
        </p>
      </header>

      {error && (
        <p className="mt-4 rounded-md border border-red-700/50 bg-red-900/20 p-3 text-sm text-red-200">
          {error}
        </p>
      )}

      <section className="mt-8">
        <h2 className="text-lg font-semibold text-ink-50">Pending submissions</h2>
        {props.pending.length === 0 ? (
          <p className="mt-3 text-sm text-ink-400">Queue is empty. Nice.</p>
        ) : (
          <div className="mt-3 space-y-3">
            {props.pending.map((s) => (
              <div key={s.id} className="card">
                <div className="flex flex-wrap items-start justify-between gap-3">
                  <div className="min-w-0">
                    <div className="font-mono text-base text-ink-50">{s.name}</div>
                    <a
                      href={s.repository}
                      target="_blank"
                      rel="noreferrer"
                      className="break-all text-xs text-accent-400 hover:text-accent-500"
                    >
                      {s.repository}@{s.ref}
                    </a>
                    {s.description && <p className="mt-2 text-sm text-ink-300">{s.description}</p>}
                    <p className="mt-2 text-xs text-ink-400">
                      Version <code className="font-mono text-ink-200">{s.version}</code>
                      {s.license && <> · {s.license}</>}
                      {s.authors && <> · by {s.authors}</>}
                      <> · submitted by @{s.submitter.login} on {new Date(s.createdAt).toLocaleString()}</>
                    </p>
                  </div>
                  <div className="flex shrink-0 gap-2">
                    <button
                      className="btn-ghost"
                      onClick={() => {
                        const reason = prompt("Reason for rejection?") ?? undefined;
                        if (reason === null) return;
                        decide(s.id, "reject", reason);
                      }}
                      disabled={busyId === s.id}
                    >
                      Reject
                    </button>
                    <button
                      className="btn-primary"
                      onClick={() => decide(s.id, "approve")}
                      disabled={busyId === s.id}
                    >
                      {busyId === s.id ? "Working…" : "Approve"}
                    </button>
                  </div>
                </div>
              </div>
            ))}
          </div>
        )}
      </section>

      <section className="mt-10">
        <h2 className="text-lg font-semibold text-ink-50">Recent submissions</h2>
        <div className="mt-3 divide-y divide-ink-800 rounded-xl border border-ink-800 bg-ink-900/40">
          {props.recent.map((s) => (
            <div key={s.id} className="flex items-center justify-between gap-3 px-4 py-3 text-sm">
              <div className="min-w-0">
                <span className="font-mono text-ink-100">{s.name}</span>
                <span className="ml-2 text-xs text-ink-400">@{s.submitter.login}</span>
              </div>
              <div className="flex items-center gap-3 text-xs">
                <span
                  className={
                    "rounded-full px-2 py-0.5 uppercase tracking-wider " +
                    (s.status === "approved"
                      ? "bg-emerald-900/30 text-emerald-200"
                      : s.status === "rejected"
                        ? "bg-red-900/30 text-red-200"
                        : "bg-ink-800 text-ink-200")
                  }
                >
                  {s.status}
                </span>
                <span className="text-ink-400">
                  {s.decidedAt ? new Date(s.decidedAt).toLocaleDateString() : new Date(s.createdAt).toLocaleDateString()}
                </span>
              </div>
            </div>
          ))}
        </div>
      </section>

      <section className="mt-10">
        <h2 className="text-lg font-semibold text-ink-50">Tweets / announcements</h2>
        <form onSubmit={createTweet} className="mt-3 card space-y-3">
          <input
            placeholder="slug (lowercase, hyphenated)"
            className="input"
            value={tweetDraft.slug}
            onChange={(e) => setTweetDraft((d) => ({ ...d, slug: e.target.value }))}
            required
          />
          <input
            placeholder="Title"
            className="input"
            value={tweetDraft.title}
            onChange={(e) => setTweetDraft((d) => ({ ...d, title: e.target.value }))}
            required
          />
          <textarea
            placeholder="Body (markdown)"
            className="input min-h-[120px]"
            value={tweetDraft.body}
            onChange={(e) => setTweetDraft((d) => ({ ...d, body: e.target.value }))}
            required
          />
          <button className="btn-primary" disabled={tweetBusy}>
            {tweetBusy ? "Posting…" : "Publish"}
          </button>
        </form>

        <div className="mt-4 divide-y divide-ink-800 rounded-xl border border-ink-800 bg-ink-900/40">
          {props.tweets.length === 0 ? (
            <p className="p-4 text-sm text-ink-400">No tweets yet.</p>
          ) : (
            props.tweets.map((t) => (
              <div key={t.id} className="flex items-center justify-between gap-3 px-4 py-3 text-sm">
                <div className="min-w-0">
                  <div className="text-ink-100">{t.title}</div>
                  <div className="text-xs text-ink-400">
                    /{t.slug} · {new Date(t.publishedAt).toLocaleString()}
                  </div>
                </div>
                <button className="btn-ghost text-xs" onClick={() => deleteTweet(t.id)}>
                  Delete
                </button>
              </div>
            ))
          )}
        </div>
      </section>
    </div>
  );
}
