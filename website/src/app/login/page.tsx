import Link from "next/link";
import { redirect } from "next/navigation";
import { getCurrentUser } from "@/lib/auth";

export const dynamic = "force-dynamic";

export default async function LoginPage({ searchParams }: { searchParams: { return_to?: string } }) {
  const user = await getCurrentUser();
  if (user) redirect(searchParams.return_to || "/");
  return (
    <div className="mx-auto max-w-md px-6 py-20 text-center">
      <h1 className="text-2xl font-semibold text-ink-50">Sign in to continue</h1>
      <p className="mt-2 text-sm text-ink-400">
        Bolide uses GitHub OAuth for sign-in. The first account to sign in
        becomes an admin.
      </p>
      <Link
        href={`/auth/login${searchParams.return_to ? `?return_to=${encodeURIComponent(searchParams.return_to)}` : ""}`}
      >
        Continue with GitHub
      </Link>
    </div>
  );
}
