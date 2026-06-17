import { NextResponse } from "next/server";
import crypto from "node:crypto";
import { getSession } from "@/lib/session";
import { githubAuthorizeUrl } from "@/lib/github-oauth";

export const dynamic = "force-dynamic";

export async function GET(req: Request) {
  const session = await getSession();
  const state = crypto.randomBytes(16).toString("hex");
  session.oauthState = state;
  const url = new URL(req.url);
  const returnTo = url.searchParams.get("return_to") || "/";
  session.oauthReturnTo = returnTo.startsWith("/") ? returnTo : "/";
  await session.save();

  const origin = process.env.SITE_ORIGIN || url.origin;
  const redirectUri = `${origin.replace(/\/+$/, "")}/auth/callback`;
  return NextResponse.redirect(githubAuthorizeUrl(state, redirectUri));
}
