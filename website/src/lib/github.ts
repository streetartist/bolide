// GitHub repository metadata + tarball utilities.
// Bolide's central index only accepts packages hosted on github.com — the
// website itself is just an indexer, and bolide-pkg downloads directly from
// codeload.github.com.

import { z } from "zod";

const GH_REPO = /^https?:\/\/github\.com\/([^/]+)\/([^/]+?)(?:\.git)?\/?$/i;

export type ParsedRepo = { owner: string; repo: string };

export function parseRepoUrl(raw: string): ParsedRepo {
  const m1 = GH_REPO.exec(raw.trim());
  if (!m1) throw new Error(`Unrecognized GitHub repository URL: ${raw}`);
  return { owner: m1[1], repo: m1[2] };
}

export function tarballUrl(parsed: ParsedRepo, ref: string): string {
  return `https://codeload.github.com/${parsed.owner}/${parsed.repo}/tar.gz/refs/tags/${ref}`;
}

const MAX_TARBALL_BYTES = 50 * 1024 * 1024; // 50 MiB safety cap.

export async function fetchTarballChecksum(parsed: ParsedRepo, ref: string): Promise<string> {
  const url = tarballUrl(parsed, ref);
  const res = await fetch(url, { redirect: "follow" });
  if (!res.ok) {
    throw new Error(`tarball GET ${url} failed: ${res.status}`);
  }
  const body = await res.arrayBuffer();
  if (body.byteLength > MAX_TARBALL_BYTES) {
    throw new Error(`tarball exceeds ${MAX_TARBALL_BYTES} bytes`);
  }
  const hash = await crypto.subtle.digest("SHA-256", body);
  return [...new Uint8Array(hash)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

const MANIFEST_RE = /^\[package\]/m;

export async function verifyRepoHasManifest(parsed: ParsedRepo, ref: string): Promise<void> {
  const url = `https://raw.githubusercontent.com/${parsed.owner}/${parsed.repo}/${ref}/bolide.toml`;
  const res = await fetch(url, { redirect: "follow" });
  if (!res.ok) {
    throw new Error(`bolide.toml not found at ${ref} in repository`);
  }
  const text = await res.text();
  if (!MANIFEST_RE.test(text)) {
    throw new Error(`bolide.toml at ${ref} is missing [package] section`);
  }
}

const RepoMeta = z.object({
  full_name: z.string(),
  description: z.string().nullable().optional(),
  html_url: z.string(),
  license: z
    .object({ spdx_id: z.string().nullable().optional() })
    .nullable()
    .optional(),
});

export type RepoMeta = z.infer<typeof RepoMeta>;

export async function fetchRepoMeta(parsed: ParsedRepo): Promise<RepoMeta> {
  const url = `https://api.github.com/repos/${parsed.owner}/${parsed.repo}`;
  const res = await fetch(url, {
    headers: {
      Accept: "application/vnd.github+json",
      "X-GitHub-Api-Version": "2022-11-28",
      "User-Agent": "bolide-website/0.1",
    },
  });
  if (!res.ok) {
    throw new Error(`repo meta fetch failed: ${res.status}`);
  }
  return RepoMeta.parse(await res.json());
}
