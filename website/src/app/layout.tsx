import "./globals.css";
import type { Metadata } from "next";
import { SiteHeader } from "@/components/SiteHeader";
import { getCurrentUser } from "@/lib/auth";

export const metadata: Metadata = {
  title: { default: "Bolide — Modern JIT/AOT Compiled Language", template: "%s · Bolide" },
  description:
    "Bolide is a modern, Cranelift-backed programming language with JIT and AOT compilation, first-class functions, async/await, FFI, and a built-in package manager.",
  metadataBase: new URL(process.env.SITE_ORIGIN || "http://localhost:3000"),
};

export default async function RootLayout({ children }: { children: React.ReactNode }) {
  const user = await getCurrentUser();
  return (
    <html lang="en">
      <body className="min-h-screen flex flex-col">
        <SiteHeader
          user={
            user
              ? { login: user.login, avatarUrl: user.avatarUrl, role: user.role }
              : null
          }
        />
        <main className="flex-1">{children}</main>
        <footer className="border-t border-ink-800 py-6 text-center text-xs text-ink-400">
          Bolide · MIT · {new Date().getFullYear()}
        </footer>
      </body>
    </html>
  );
}
