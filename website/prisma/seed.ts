// Seed script: create a default admin user (only if there are no users) and
// a few sample tweets. Run with `npm run db:seed` after `npm run db:push`.

import { PrismaClient } from "@prisma/client";

const prisma = new PrismaClient();

async function main() {
  const userCount = await prisma.user.count();
  console.log(`users: ${userCount}`);
  if (userCount === 0) {
    // Bootstrap a placeholder admin so the admin UI is reachable before any
    // OAuth callback fires. The real OAuth flow will upsert by githubId and
    // can replace this record's id by editing the DB to match the OAuth
    // identity, or by deleting it and signing in fresh.
    const u = await prisma.user.create({
      data: {
        githubId: 0,
        login: "bootstrap",
        role: "admin",
      },
    });
    console.log(`created bootstrap admin id=${u.id} (replace by signing in via GitHub OAuth)`);
  }
  // No-op for tweets: the admin UI is the right place to publish them.
}

main()
  .catch((e) => {
    console.error(e);
    process.exit(1);
  })
  .finally(() => prisma.$disconnect());
