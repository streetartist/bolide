# bolide-website

The official website for the Bolide programming language.

- **/** — homepage, feature grid, latest tweets
- **/download** — release binaries (Windows / Linux / macOS)
- **/packages** — public package index (search + list)
- **/packages/[name]** — package detail with install instructions
- **/submit** — sign-in-gated form to propose a new package
- **/admin** — admin-only moderation queue, tweet publisher
- **/tweets** — public announcements
- **/auth/{login,callback,logout,me}** — GitHub App OAuth web flow
- **/api/packages{,/submit,/[name]}** — JSON API for the public index
- **/api/admin/{submissions,tweets}** — JSON API for the admin panel
- **/package/[...path]** — wire-protocol endpoint consumed by `bolide-pkg`
  (returns `{name, versions:[{version, checksum, download_url}]}`)
- **/webhook** — GitHub webhook receiver (`X-Hub-Signature-256` verified)

The site does **not** host package bytes. The index is metadata only;
bolide-pkg downloads tarballs directly from the URLs in the index
(`codeload.github.com/.../tar.gz/refs/tags/<tag>`).

## Stack

- Next.js 14 (App Router) + React 18
- Tailwind CSS
- Prisma + SQLite
- iron-session (encrypted cookie session, no external session store)
- GitHub App OAuth (web flow, scopes: `read:user user:email`)

## Local dev

```bash
cd website
npm install
cp .env.example .env
# Edit .env: fill SESSION_PASSWORD and GITHUB_OAUTH_CLIENT_SECRET.
npm run db:push
npm run dev
```

Open <http://localhost:3000>.

## GitHub App setup

1. Create a GitHub App at <https://github.com/settings/apps/new>:
   - **GitHub App name**: Bolide Website
   - **Homepage URL**: `https://bolide.streetartist.top`
   - **Callback URL**: `https://bolide.streetartist.top/auth/callback`
   - **Request user authorization (OAuth) during installation**: ON
   - **Webhook URL**: `https://bolide.streetartist.top/webhook`
   - **Webhook secret**: a random string; copy it to `GITHUB_WEBHOOK_SECRET`
2. On the App's page, **Generate a new client secret** and paste both
   `Client ID` and the secret into `.env`
   (`GITHUB_OAUTH_CLIENT_ID`, `GITHUB_OAUTH_CLIENT_SECRET`).
3. Subscribe the App to webhook events you care about (e.g. `release`,
   `push`); the receiver at `/webhook` accepts any event, verifies
   `X-Hub-Signature-256`, and persists it for later processing.

The scope on the OAuth screen is `read:user user:email` — we do **not**
request `repo`, so users can sign in without granting code access to their
private repos. The first user to complete the OAuth flow becomes an admin;
subsequent users default to `user` and must be promoted manually:

```bash
npx prisma studio
# → edit User.role to "admin"
```

## Wiring bolide-pkg to this registry

The `IndexEntry` shape served at `/package/<prefix>/<name>.json` is
byte-compatible with `crates/bolide-pkg/src/registry.rs::IndexEntry`:

```json
{
  "name": "http",
  "versions": [
    {
      "version": "0.1.0",
      "checksum": "<sha256-hex>",
      "download_url": "https://codeload.github.com/owner/http/tar.gz/refs/tags/v0.1.0"
    }
  ]
}
```

Users on the consumer side point the registry at this host in `bolide.toml`:

```toml
[dependencies]
http = { version = "0.1.0", registry = "https://bolide.streetartist.top" }
```

## Building a release tarball

```bash
cd website
npm run package
```

This runs `scripts/package-release.js` which:

1. `next build` with `output: "standalone"` (self-contained Node server)
2. Copies the standalone output + client assets + Prisma CLI into a staging directory
3. Strips query engine binaries that don't match the build host's OS so
   `prisma generate` on the deploy host downloads the correct one
4. Adds `start.sh` (Linux) / `start.ps1` (Windows) launchers + `README.deploy.md`
5. Creates `dist/bolide-website-<version>.tar.gz`

The resulting tarball only requires `node` >= 18.17 on the deploy host.

## Deploy to Linux server

### 1. Upload

```bash
scp dist/bolide-website-0.1.0.tar.gz user@bolide.streetartist.top:/tmp/
```

### 2. Extract and configure

```bash
ssh user@bolide.streetartist.top

sudo mkdir -p /opt/bolide-website
sudo chown $USER:$USER /opt/bolide-website
cd /opt/bolide-website
tar -xzf /tmp/bolide-website-0.1.0.tar.gz --strip-components=1
```

Create `.env` in the project root (secrets — never commit):

```env
DATABASE_URL="file:./prisma/dev.db"
SESSION_PASSWORD="<random 32+ hex chars>"

GITHUB_OAUTH_CLIENT_ID="Iv23lim6CDJaNWw02bQS"
GITHUB_OAUTH_CLIENT_SECRET="<from GitHub App settings>"

SITE_ORIGIN="https://bolide.streetartist.top"
GITHUB_WEBHOOK_SECRET="<same as the GitHub App webhook secret>"
```

Generate `SESSION_PASSWORD`:

```bash
node -e "console.log(require('crypto').randomBytes(32).toString('hex'))"
```

### 3. First start

```bash
chmod +x start.sh
./start.sh
```

`start.sh` does the following on every start:

1. Loads `.env` (or `/etc/bolide-website.env`)
2. Patches absolute paths in `.next/required-server-files.json` to match
   the actual install directory (the build machine's paths are baked in
   during `next build` and must be rewritten on the deploy host)
3. If `prisma/dev.db` does not exist yet, runs `prisma generate` (downloads
   the correct query engine for the current platform) then `prisma db push`
   (initializes the SQLite schema)
4. Starts `node server.js`

Alternatively, skip the launcher and start directly:

```bash
node server.js
```

(The path patch + DB init steps must be done manually if you don't use
`start.sh`.)

### 4. Sign in

Open `https://bolide.streetartist.top` in a browser and click **Sign in**.
The first GitHub user to complete the OAuth flow becomes an admin
automatically.

### 5. Reverse proxy (Caddy)

```
bolide.streetartist.top {
    reverse_proxy 127.0.0.1:3000
    encode zstd gzip
}
```

For nginx, use:

```nginx
server {
    listen 443 ssl http2;
    server_name bolide.streetartist.top;

    ssl_certificate     /etc/letsencrypt/live/bolide.streetartist.top/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/bolide.streetartist.top/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:3000;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

### 6. systemd (recommended for production)

Create `/etc/systemd/system/bolide-website.service`:

```ini
[Unit]
Description=Bolide website
After=network.target

[Service]
Type=simple
User=bolide
WorkingDirectory=/opt/bolide-website
EnvironmentFile=/opt/bolide-website/.env
ExecStart=/usr/bin/node server.js
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now bolide-website
sudo journalctl -u bolide-website -f
```

### 7. Backup

The entire database is a single file:

```bash
cp /opt/bolide-website/prisma/dev.db /backups/bolide-$(date +%F).db
```

### 8. Updating

Rebuild on the build machine, then deploy:

```bash
# Build machine
cd website
npm run package

# Deploy
scp dist/bolide-website-<new>.tar.gz server:/tmp/
ssh server 'sudo systemctl stop bolide-website && \
            cd /opt/bolide-website && tar -xzf /tmp/bolide-website-<new>.tar.gz --strip-components=1 --overwrite && \
            sudo systemctl start bolide-website'
```

If the Prisma schema changed, `start.sh` will detect the missing (or stale)
DB and run `prisma db push` automatically on next start.

### 9. Promoting additional admins

The first user to sign in via GitHub becomes admin. To promote others:

```bash
cd /opt/bolide-website
node -e "
  const { PrismaClient } = require('@prisma/client');
  const p = new PrismaClient();
  p.user.update({ where: { login: 'someone' }, data: { role: 'admin' } })
    .then(() => p.\$disconnect());
"
```

Or use `npx prisma studio` for a visual editor.
