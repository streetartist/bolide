import Link from "next/link";
import { redirect } from "next/navigation";
import { getCurrentUser } from "@/lib/auth";
import { SubmitForm } from "./SubmitForm";

export const dynamic = "force-dynamic";

export default async function SubmitPage() {
  const user = await getCurrentUser();
  if (!user) redirect("/login?return_to=/submit");

  return (
    <div className="mx-auto max-w-2xl px-6 py-12">
      <h1 className="text-3xl font-semibold text-ink-50">Submit a package</h1>
      <p className="mt-2 text-ink-400">
        Point us at a GitHub repository. The submission goes into the moderation queue
        and is reviewed by a Bolide maintainer before it appears in the public index.
      </p>
      <div className="mt-6 rounded-md border border-ink-800 bg-ink-900/40 p-4 text-sm text-ink-400">
        <p className="font-semibold text-ink-200">What we verify before approval:</p>
        <ul className="mt-2 list-disc space-y-1 pl-5">
          <li>Repository contains a <code className="font-mono text-ink-200">bolide.toml</code> with a valid <code className="font-mono text-ink-200">[package]</code> section at the ref you submit.</li>
          <li>License is set, or you supply one in the form.</li>
          <li>Source tarball is reachable from our build host (so the sha256 we publish actually matches the bytes bolide-pkg will download).</li>
        </ul>
      </div>

      <SubmitForm />

      <p className="mt-6 text-xs text-ink-400">
        Need to update an existing package? Sign in as the original submitter and{" "}
        <Link href="/admin" className="text-accent-400 hover:text-accent-500">
          contact a maintainer
        </Link>
        .
      </p>
    </div>
  );
}
