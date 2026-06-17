#!/usr/bin/env node
/**
 * Build a self-contained deployment tarball for the Bolide website.
 *
 *   node scripts/package-release.js
 */

const fs = require("node:fs");
const path = require("node:path");
const zlib = require("node:zlib");
const { spawnSync } = require("node:child_process");

const ROOT = path.resolve(__dirname, "..");
const DIST = path.join(ROOT, "dist");
const VERSION = JSON.parse(fs.readFileSync(path.join(ROOT, "package.json"), "utf8")).version;
const TARBALL = path.join(DIST, "bolide-website-" + VERSION + ".tar.gz");
const STAGE = path.join(DIST, "bolide-website-" + VERSION);

const START_SH = [
  "#!/bin/sh",
  "# Bolide website launcher.",
  "# Reads .env from the current directory (or /etc/bolide-website.env) and",
  "# starts the standalone Next.js server.",
  "",
  "set -e",
  "",
  'SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"',
  'cd "$SCRIPT_DIR"',
  "",
  "if [ -f .env ]; then",
  "  set -a; . ./.env; set +a",
  "elif [ -f /etc/bolide-website.env ]; then",
  "  set -a; . /etc/bolide-website.env; set +a",
  "fi",
  "",
  "# Fix absolute paths baked in during build so Next.js finds the build",
  "# in its actual install directory, not the build machine's path.",
  'node -e "',
  '  const f = require(\"fs\"), p = require(\"path\");',
  '  const d = process.cwd(), j = p.join(d, \".next\", \"required-server-files.json\");',
  '  if (!f.existsSync(j)) process.exit(0);',
  '  const s = f.readFileSync(j, \"utf8\");',
  "  let c = JSON.parse(s);",
  "  c.appDir = d;",
  "  if (c.config?.experimental?.outputFileTracingRoot) c.config.experimental.outputFileTracingRoot = d;",
  '  f.writeFileSync(j, JSON.stringify(c));',
  "  console.log(\">> Patched appDir/outputFileTracingRoot to:\", d);",
  '"',
  "",
  "mkdir -p logs",
  'export PORT="${PORT:-3000}"',
  'export HOSTNAME="${HOSTNAME:-0.0.0.0}"',
  "",
  'if [ ! -f prisma/dev.db ] && [ "${DATABASE_URL:-}" = "file:./prisma/dev.db" -o -z "${DATABASE_URL:-}" ]; then',
  '  echo ">> Generating Prisma client for this platform..."',
  "  node node_modules/prisma/build/index.js generate",
  '  echo ">> Initializing database schema..."',
  "  node node_modules/prisma/build/index.js db push --skip-generate",
  "fi",
  "",
  'echo ">> Starting bolide-website on http://${HOSTNAME}:${PORT}"',
  "exec node server.js",
  "",
].join("\n");

const START_PS1 = [
  "# Bolide website launcher (Windows).",
  '# Reads .env from the current directory and starts the standalone server.',
  '$ErrorActionPreference = "Stop"',
  '$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path',
  'Set-Location $ScriptDir',
  "",
  "if (Test-Path .env) {",
  "  Get-Content .env | ForEach-Object {",
  '    if ($_ -match "^\\s*([^#][^=]*)=(.*)$") {',
  "      [Environment]::SetEnvironmentVariable($matches[1].Trim(), $matches[2].Trim(), 'Process')",
  "    }",
  "  }",
  "}",
  "",
  '# Fix absolute paths baked in during build.',
  "node -e \"",
  '  const f = require(\"fs\"), p = require(\"path\");',
  '  const d = process.cwd(), j = p.join(d, \".next\", \"required-server-files.json\");',
  '  if (!f.existsSync(j)) process.exit(0);',
  '  const s = f.readFileSync(j, \"utf8\");',
  "  let c = JSON.parse(s);",
  "  c.appDir = d;",
  "  if (c.config?.experimental?.outputFileTracingRoot) c.config.experimental.outputFileTracingRoot = d;",
  '  f.writeFileSync(j, JSON.stringify(c));',
  '  console.log(\">> Patched appDir/outputFileTracingRoot to:\", d);',
  '"',
  "",
  'if (-not (Test-Path prisma\\dev.db) -and "$env:DATABASE_URL" -eq "file:./prisma/dev.db") {',
  '  Write-Host ">> Generating Prisma client for this platform..."',
  "  node node_modules\\prisma\\build\\index.js generate",
  '  Write-Host ">> Initializing database schema..."',
  "  node node_modules\\prisma\\build\\index.js db push --skip-generate",
  "}",
  "",
  'if (-not $env:PORT) { $env:PORT = "3000" }',
  'if (-not $env:HOSTNAME) { $env:HOSTNAME = "0.0.0.0" }',
  "",
  'Write-Host ">> Starting bolide-website on http://$env:HOSTNAME`:$env:PORT"',
  "node server.js",
  "",
].join("\n");

const DEPLOY_README = [
  "# Bolide Website - Deployment",
  "",
  "Self-contained Next.js `standalone` build. Requires only `node` >= 18.17.",
  "",
  "## First-time setup",
  "",
  "1. Extract the tarball and enter the directory:",
  "",
  "   ```bash",
  "   tar -xzf bolide-website-0.1.0.tar.gz",
  "   cd bolide-website-0.1.0",
  "   ```",
  "",
  "2. Create `.env` (secrets - never commit):",
  "",
  "   ```env",
  '   DATABASE_URL="file:./prisma/dev.db"',
  '   SESSION_PASSWORD="<32+ random hex chars>"',
  "",
  '   GITHUB_OAUTH_CLIENT_ID="Iv23lim6CDJaNWw02bQS"',
  '   GITHUB_OAUTH_CLIENT_SECRET="<from GitHub App settings>"',
  "",
  '   SITE_ORIGIN="https://bolide.streetartist.top"',
  '   GITHUB_WEBHOOK_SECRET="<shared secret with the GitHub App>"',
  "   ```",
  "",
  "   Generate `SESSION_PASSWORD`:",
  "   ```bash",
  "   node -e \"console.log(require('crypto').randomBytes(32).toString('hex'))\"",
  "   ```",
  "",
  "3. Start the server. On first run `start.sh` patches absolute paths baked",
  "   in during build, downloads the right Prisma engine for your platform",
  "   (if it differs from the build host), and initializes the SQLite schema:",
  "",
  "   ```bash",
  "   chmod +x start.sh",
  "   ./start.sh",
  "   ```",
  "",
  "   Or start directly with `node server.js` (skip the patches above).",
  "",
  "The **first GitHub user to sign in becomes an admin automatically**.",
  "",
  "## Reverse proxy (Caddy example)",
  "",
  "```",
  "bolide.streetartist.top {",
  "    reverse_proxy 127.0.0.1:3000",
  "    encode zstd gzip",
  "}",
  "```",
  "",
  "## systemd (recommended for production)",
  "",
  "```ini",
  "# /etc/systemd/system/bolide-website.service",
  "[Unit]",
  "Description=Bolide website",
  "After=network.target",
  "",
  "[Service]",
  "Type=simple",
  "User=bolide",
  "WorkingDirectory=/opt/bolide-website",
  "EnvironmentFile=/etc/bolide-website.env",
  "ExecStart=/usr/bin/node server.js",
  "Restart=on-failure",
  "RestartSec=5",
  "",
  "[Install]",
  "WantedBy=multi-user.target",
  "```",
  "",
  "```bash",
  "sudo systemctl daemon-reload",
  "sudo systemctl enable --now bolide-website",
  "sudo journalctl -u bolide-website -f",
  "```",
  "",
  "## GitHub App configuration",
  "",
  "In the GitHub App settings (https://github.com/settings/apps):",
  "",
  "- **Homepage URL**: `https://bolide.streetartist.top`",
  "- **Callback URL**: `https://bolide.streetartist.top/auth/callback`",
  "- **Request user authorization (OAuth) during installation**: ON",
  "- **Webhook URL**: `https://bolide.streetartist.top/webhook`",
  "- **Webhook secret**: same as `GITHUB_WEBHOOK_SECRET` in the env",
  "",
  "The first GitHub user to sign in becomes admin. To promote additional",
  "users:",
  "",
  "```bash",
  "node -e \"",
  "  const { PrismaClient } = require('@prisma/client');",
  "  const p = new PrismaClient();",
  "  p.user.update({ where: { login: 'someone' }, data: { role: 'admin' } })",
  "    .then(() => p.\\$disconnect());",
  "\"",
  "```",
  "",
  "## Backup",
  "",
  "Just copy `prisma/dev.db` - that's the entire database.",
  "",
  "## Updating",
  "",
  "```bash",
  "# On the build machine",
  "node scripts/package-release.js",
  "scp dist/bolide-website-<new>.tar.gz server:/tmp/",
  "ssh server 'sudo systemctl stop bolide-website && \\",
  "            cd /opt && tar -xzf /tmp/bolide-website-<new>.tar.gz --overwrite && \\",
  "            sudo systemctl start bolide-website'",
  "```",
  "",
  "If the schema changed, run `prisma db push` once before restarting.",
  "",
].join("\n");

function rmrf(p) {
  fs.rmSync(p, { recursive: true, force: true });
}

function copyDir(src, dest) {
  fs.mkdirSync(dest, { recursive: true });
  for (const entry of fs.readdirSync(src, { withFileTypes: true })) {
    const s = path.join(src, entry.name);
    const d = path.join(dest, entry.name);
    if (entry.isDirectory()) copyDir(s, d);
    else if (entry.isFile()) fs.copyFileSync(s, d);
  }
}

function buildStandalone() {
  console.log(">> Building standalone bundle...");
  const r = spawnSync(
    process.execPath,
    [path.join(ROOT, "node_modules", "next", "dist", "bin", "next"), "build"],
    { cwd: ROOT, stdio: "inherit" },
  );
  if (r.status !== 0) throw new Error("next build failed");
}

function stripNonHostEngines(stage) {
  const hostOs = process.platform;
  const hostArch = process.arch;
  const OS_TOKEN = { win32: "windows", linux: "linux", darwin: "darwin" }[hostOs];
  const isHostEntry = (entry) => {
    const low = entry.toLowerCase();
    if (!low.includes(OS_TOKEN)) return false;
    if (hostArch === "x64" && low.includes("arm64")) return false;
    if (hostArch === "arm64" && low.includes("x64") && !low.includes("arm64")) return false;
    return true;
  };
  const isForeign = (entry) =>
    /^(query_engine|schema-engine)-/.test(entry) && !isHostEntry(entry);

  const roots = [
    path.join(stage, "node_modules", ".prisma", "client"),
    path.join(stage, "node_modules", "@prisma", "engines"),
    path.join(stage, "node_modules", "prisma"),
  ];
  for (const dir of roots) {
    if (!fs.existsSync(dir)) continue;
    for (const entry of fs.readdirSync(dir)) {
      if (isForeign(entry)) {
        fs.rmSync(path.join(dir, entry), { force: true });
        console.log(">> stripped non-host engine: " + path.relative(stage, path.join(dir, entry)));
      }
    }
  }
}

function stage() {
  console.log(">> Staging " + STAGE + " ...");
  rmrf(STAGE);
  fs.mkdirSync(STAGE, { recursive: true });

  copyDir(path.join(ROOT, ".next", "standalone"), STAGE);
  copyDir(path.join(ROOT, ".next", "static"), path.join(STAGE, ".next", "static"));

  copyDir(
    path.join(ROOT, "node_modules", "@prisma"),
    path.join(STAGE, "node_modules", "@prisma"),
  );
  copyDir(
    path.join(ROOT, "node_modules", "prisma"),
    path.join(STAGE, "node_modules", "prisma"),
  );

  stripNonHostEngines(STAGE);

  const prismaDest = path.join(STAGE, "prisma");
  fs.mkdirSync(prismaDest, { recursive: true });
  fs.copyFileSync(path.join(ROOT, "prisma", "schema.prisma"), path.join(prismaDest, "schema.prisma"));
  fs.copyFileSync(path.join(ROOT, "prisma", "seed.ts"), path.join(prismaDest, "seed.ts"));

  fs.writeFileSync(path.join(STAGE, "start.sh"), START_SH, { mode: 0o755 });
  fs.writeFileSync(path.join(STAGE, "start.ps1"), START_PS1);
  fs.writeFileSync(path.join(STAGE, "README.deploy.md"), DEPLOY_README);
}

function makeTarball() {
  console.log(">> Creating " + TARBALL + " ...");
  rmrf(TARBALL);
  const stageName = path.basename(STAGE);
  const r = spawnSync("tar", ["-czf", TARBALL, "-C", path.dirname(STAGE), stageName], {
    stdio: "inherit",
  });
  if (r.status !== 0) {
    console.warn(">> system tar failed, using Node fallback");
    tarGz(STAGE, TARBALL);
  }
  const size = fs.statSync(TARBALL).size;
  console.log(">> Built " + TARBALL + " (" + (size / 1024 / 1024).toFixed(2) + " MB)");
}

function tarGz(srcDir, outFile) {
  const files = [];
  function walk(dir, prefix) {
    for (const name of fs.readdirSync(dir)) {
      const abs = path.join(dir, name);
      const st = fs.statSync(abs);
      if (st.isDirectory()) walk(abs, prefix + name + "/");
      else files.push({ abs, name: prefix + name, size: st.size });
    }
  }
  walk(srcDir, path.basename(srcDir) + "/");

  const blocks = [];
  for (const f of files) {
    const header = Buffer.alloc(512);
    const nm = Buffer.from(f.name);
    nm.copy(header, 0, 0, Math.min(nm.length, 100));
    header.write("0000644 ", 100, "8", "ascii");
    header.write("0001750 ", 108, "8", "ascii");
    header.write("0000000 ", 116, "8", "ascii");
    header.write(f.size.toString(8).padStart(11, "0") + " ", 124, "12", "ascii");
    header.write(Math.floor(Date.now() / 1000).toString(8).padStart(11, "0") + " ", 136, "12", "ascii");
    header.write("        ", 148, "8", "ascii");
    header.write("0", 156, "1", "ascii");
    let sum = 0;
    for (let i = 0; i < 512; i++) sum += header[i];
    header.write(sum.toString(8).padStart(6, "0") + "\0 ", 148, "8", "ascii");
    blocks.push(header);
    blocks.push(fs.readFileSync(f.abs));
    const pad = (512 - (f.size % 512)) % 512;
    if (pad) blocks.push(Buffer.alloc(pad));
  }
  blocks.push(Buffer.alloc(1024));
  const gz = zlib.gzipSync(Buffer.concat(blocks), { level: 9 });
  fs.writeFileSync(outFile, gz);
}

function main() {
  fs.mkdirSync(DIST, { recursive: true });
  buildStandalone();
  stage();
  makeTarball();
  console.log("\nDone. Upload to server with:");
  console.log("  scp " + TARBALL + " user@server:/tmp/");
}

main();
