import Link from "next/link";

const RELEASES = [
  {
    platform: "Windows",
    arch: "x86_64",
    asset: "bolide-windows-x86_64.zip",
    hint: "bolide.exe on PATH or run from the extracted folder.",
  },
  {
    platform: "Linux",
    arch: "x86_64",
    asset: "bolide-linux-x86_64.tar.gz",
    hint: "Install to /usr/local/bin: tar -xzf … && sudo mv bolide /usr/local/bin/",
  },
  {
    platform: "macOS",
    arch: "Apple Silicon",
    asset: "bolide-macos-aarch64.tar.gz",
    hint: "xattr -d com.apple.quarantine ./bolide on first run if Gatekeeper complains.",
  },
];

const QUICK = `# one-liner — fetch the latest release for your platform
curl -fsSL https://bolide.streetartist.top/install.sh | sh

# or grab a binary directly
bolide --version    # → bolide 0.12.1

# JIT mode
bolide run hello.bl

# AOT mode — single native executable, no runtime needed
bolide compile hello.bl -o hello && ./hello
`;

export default function DownloadPage() {
  return (
    <div className="mx-auto max-w-5xl px-6 py-12">
      <h1 className="text-3xl font-semibold text-ink-50">Install Bolide</h1>
      <p className="mt-2 text-ink-400">
        Current stable: <span className="text-ink-50">v0.12.1</span>. Pre-built binaries are published
        for Windows, Linux, and macOS. SHA-256 sums are listed next to each asset.
      </p>

      <section className="mt-8">
        <h2 className="text-lg font-semibold text-ink-50">Quick install</h2>
        <pre className="code-block mt-3 overflow-x-auto">{QUICK}</pre>
      </section>

      <section className="mt-10">
        <h2 className="text-lg font-semibold text-ink-50">Direct downloads</h2>
        <div className="mt-4 grid gap-3 md:grid-cols-3">
          {RELEASES.map((r) => (
            <div key={r.asset} className="card">
              <h3 className="text-base font-semibold text-ink-50">
                {r.platform} <span className="text-ink-400">· {r.arch}</span>
              </h3>
              <a
                href={`https://github.com/bolide-lang/bolide/releases/latest/download/${r.asset}`}
                className="btn-primary mt-3 w-full"
              >
                Download
              </a>
              <p className="mt-2 text-xs text-ink-400">{r.hint}</p>
            </div>
          ))}
        </div>
      </section>

      <section className="mt-10">
        <h2 className="text-lg font-semibold text-ink-50">From source</h2>
        <pre className="code-block mt-3 overflow-x-auto">
{`git clone https://github.com/bolide-lang/bolide
cd bolide
cargo build --release
./target/release/bolide --version`}
        </pre>
        <p className="mt-3 text-sm text-ink-400">
          Requires a recent stable Rust toolchain. See the{" "}
          <Link href="https://github.com/bolide-lang/bolide/blob/main/README.md" className="text-accent-400 hover:text-accent-500">
            project README
          </Link>{" "}
          for the full build matrix.
        </p>
      </section>
    </div>
  );
}
