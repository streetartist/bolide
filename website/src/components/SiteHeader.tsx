"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";

type Props = {
  user: { login: string; avatarUrl: string | null; role: string } | null;
};

const NAV = [
  { href: "/", label: "Home" },
  { href: "/packages", label: "Packages" },
  { href: "/download", label: "Download" },
  { href: "/tweets", label: "Tweets" },
];

export function SiteHeader({ user }: Props) {
  const pathname = usePathname();
  return (
    <header className="border-b border-ink-800 bg-ink-950/80 backdrop-blur sticky top-0 z-30">
      <div className="mx-auto flex max-w-6xl items-center justify-between gap-4 px-6 py-3">
        <Link href="/" className="flex items-center gap-2 font-semibold tracking-tight text-ink-50">
          {/* eslint-disable-next-line @next/next/no-img-element */}
          <img src="/bolide_logo.png" alt="Bolide" className="h-7 w-7 rounded-full" />
          Bolide
        </Link>
        <nav className="hidden gap-1 md:flex">
          {NAV.map((n) => {
            const active = n.href === "/" ? pathname === "/" : pathname.startsWith(n.href);
            return (
              <Link
                key={n.href}
                href={n.href}
                className={
                  "rounded-md px-3 py-1.5 text-sm transition " +
                  (active
                    ? "bg-ink-900 text-ink-50"
                    : "text-ink-400 hover:bg-ink-900 hover:text-ink-50")
                }
              >
                {n.label}
              </Link>
            );
          })}
        </nav>
        <div className="flex items-center gap-2">
          {user ? (
            <>
              {user.role === "admin" && (
                <Link href="/admin" className="btn-ghost">
                  Admin
                </Link>
              )}
              <Link href="/submit" className="btn-primary">
                Submit package
              </Link>
              <Link
                href="/auth/logout"
                className="flex items-center gap-2 text-sm text-ink-400 hover:text-ink-50"
                title={`Signed in as ${user.login}`}
              >
                {user.avatarUrl ? (
                  // eslint-disable-next-line @next/next/no-img-element
                  <img
                    src={user.avatarUrl}
                    alt={user.login}
                    className="h-7 w-7 rounded-full border border-ink-800"
                  />
                ) : (
                  <span className="h-7 w-7 rounded-full bg-ink-800" />
                )}
                <span className="hidden sm:inline">{user.login}</span>
              </Link>
            </>
          ) : (
            <Link href="/auth/login?return_to=/submit" className="btn-primary">
              Sign in
            </Link>
          )}
        </div>
      </div>
    </header>
  );
}
