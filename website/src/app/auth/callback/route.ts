import { NextResponse } from "next/server";
import { getSession } from "@/lib/session";
import { prisma } from "@/lib/prisma";
import { exchangeCodeForToken, fetchGithubUser } from "@/lib/github-oauth";

export const dynamic = "force-dynamic";

export async function GET(req: Request) {
  const session = await getSession();
  const url = new URL(req.url);
  const code = url.searchParams.get("code");
  const state = url.searchParams.get("state");
  const err = url.searchParams.get("error");

  if (err) {
    return NextResponse.redirect(new URL(`/?oauth_error=${encodeURIComponent(err)}`, req.url));
  }
  if (!code || !state || state !== session.oauthState) {
    return NextResponse.redirect(new URL("/?oauth_error=state_mismatch", req.url));
  }
  session.oauthState = undefined;
  const returnTo = session.oauthReturnTo || "/";
  session.oauthReturnTo = undefined;

  const origin = process.env.SITE_ORIGIN || url.origin;
  const redirectUri = `${origin.replace(/\/+$/, "")}/auth/callback`;

  const token = await exchangeCodeForToken(code, redirectUri);
  const ghUser = await fetchGithubUser(token);

  // First-ever user becomes admin. Subsequent users default to "user" and can
  // be promoted manually in the DB.
  const userCount = await prisma.user.count();
  const isFirst = userCount === 0;

  const user = await prisma.user.upsert({
    where: { githubId: ghUser.id },
    create: {
      githubId: ghUser.id,
      login: ghUser.login,
      email: ghUser.email ?? null,
      avatarUrl: ghUser.avatar_url ?? null,
      role: isFirst ? "admin" : "user",
    },
    update: {
      login: ghUser.login,
      email: ghUser.email ?? null,
      avatarUrl: ghUser.avatar_url ?? null,
    },
  });

  session.userId = user.id;
  session.githubId = user.githubId;
  session.login = user.login;
  await session.save();

  return NextResponse.redirect(new URL(returnTo, req.url));
}
