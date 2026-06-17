import Link from "next/link";

export default function NotFound() {
  return (
    <div className="mx-auto max-w-md px-6 py-24 text-center">
      <h1 className="text-3xl font-semibold text-ink-50">404</h1>
      <p className="mt-2 text-ink-400">This page does not exist.</p>
      <Link href="/" className="btn-ghost mt-6">
        Back home
      </Link>
    </div>
  );
}
