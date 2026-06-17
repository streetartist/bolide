"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";

type State =
  | { kind: "idle" }
  | { kind: "submitting" }
  | { kind: "ok"; id: string }
  | { kind: "error"; message: string };

export function SubmitForm() {
  const router = useRouter();
  const [state, setState] = useState<State>({ kind: "idle" });

  async function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const fd = new FormData(e.currentTarget);
    const body = {
      name: String(fd.get("name") ?? "").trim(),
      repository: String(fd.get("repository") ?? "").trim(),
      ref: String(fd.get("ref") ?? "main").trim() || "main",
      version: String(fd.get("version") ?? "").trim(),
      description: String(fd.get("description") ?? "").trim() || undefined,
      homepage: String(fd.get("homepage") ?? "").trim() || undefined,
      license: String(fd.get("license") ?? "").trim() || undefined,
      authors: String(fd.get("authors") ?? "").trim() || undefined,
    };

    setState({ kind: "submitting" });
    try {
      const res = await fetch("/api/packages/submit", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      const data = await res.json();
      if (!res.ok || !data.ok) {
        setState({ kind: "error", message: data.error || `HTTP ${res.status}` });
        return;
      }
      setState({ kind: "ok", id: data.submissionId });
      router.refresh();
    } catch (err) {
      setState({ kind: "error", message: (err as Error).message });
    }
  }

  if (state.kind === "ok") {
    return (
      <div className="mt-6 rounded-md border border-emerald-700/50 bg-emerald-900/20 p-4 text-sm text-emerald-200">
        Submission <code className="font-mono">{state.id}</code> received. A maintainer will review
        it shortly — you can track its status from the admin panel.
      </div>
    );
  }

  return (
    <form className="mt-6 space-y-4" onSubmit={onSubmit}>
      <Field name="name" label="Package name" required pattern="^[a-z0-9][a-z0-9_-]*$" hint="Lowercase, no spaces. Used as the namespace in `import`." />
      <Field name="repository" label="Repository URL" required type="url" placeholder="https://github.com/owner/repo" />
      <div className="grid gap-4 sm:grid-cols-2">
        <Field name="ref" label="Git ref" placeholder="main" hint="Tag, branch, or commit SHA. Must contain bolide.toml." />
        <Field name="version" label="Version" required placeholder="0.1.0" />
      </div>
      <Field name="description" label="Description" />
      <div className="grid gap-4 sm:grid-cols-2">
        <Field name="homepage" label="Homepage (optional)" type="url" />
        <Field name="license" label="License (SPDX id)" placeholder="MIT" />
      </div>
      <Field name="authors" label="Authors" placeholder="Jane Doe <[email protected]>" />

      {state.kind === "error" && (
        <p className="rounded-md border border-red-700/50 bg-red-900/20 p-3 text-sm text-red-200">
          {state.message}
        </p>
      )}

      <button
        type="submit"
        className="btn-primary w-full"
        disabled={state.kind === "submitting"}
      >
        {state.kind === "submitting" ? "Submitting…" : "Submit for review"}
      </button>
    </form>
  );
}

function Field({
  name,
  label,
  required,
  type = "text",
  placeholder,
  pattern,
  hint,
}: {
  name: string;
  label: string;
  required?: boolean;
  type?: string;
  placeholder?: string;
  pattern?: string;
  hint?: string;
}) {
  return (
    <label className="block">
      <span className="text-sm text-ink-200">{label}</span>
      <input
        name={name}
        type={type}
        required={required}
        pattern={pattern}
        placeholder={placeholder}
        className="input mt-1"
      />
      {hint && <span className="mt-1 block text-xs text-ink-400">{hint}</span>}
    </label>
  );
}
