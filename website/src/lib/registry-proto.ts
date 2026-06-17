// Wire types matching bolide-pkg's `IndexEntry` / `IndexVersion`.
// `crates/bolide-pkg/src/registry.rs` expects:
//   { name: String, versions: [{ version, checksum, download_url }] }

import { prisma } from "./prisma";

export type IndexVersionWire = {
  version: string;
  checksum: string;
  download_url: string;
};

export type IndexEntryWire = {
  name: string;
  versions: IndexVersionWire[];
};

// Cargo-style shallow URL layout, identical to bolide-pkg's `index_url`:
//   <2>/<3>/<name>  when name length >= 3, else <1>/<name>.
export function indexPathFor(name: string): string {
  const lower = name.toLowerCase();
  if (lower.length >= 3) return `/${lower.slice(0, 2)}/${lower.slice(2, 3)}/${name}`;
  return `/${lower.slice(0, 1)}/${name}`;
}

export async function buildIndexEntry(name: string): Promise<IndexEntryWire | null> {
  const pkg = await prisma.package.findUnique({
    where: { name },
    include: { versions: { where: { yank: false }, orderBy: { createdAt: "desc" } } },
  });
  if (!pkg || pkg.status !== "approved") return null;
  return {
    name: pkg.name,
    versions: pkg.versions.map((v) => ({
      version: v.version,
      checksum: v.checksum,
      download_url: v.downloadUrl,
    })),
  };
}
