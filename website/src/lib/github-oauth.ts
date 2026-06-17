// GitHub App OAuth helpers (web flow).
//   GET  https://github.com/login/oauth/authorize?client_id=...&redirect_uri=...&state=...&scope=user:email
//   POST https://github.com/login/oauth/access_token   (form-encoded, returns access_token)
//   GET  https://api.github.com/user                  (Authorization: Bearer <token>)
//
// Note: client_id is enough — we do NOT pass `app_id` in the URL. `app_id` is
// only relevant when "acting as the app" via installation tokens, which we
// don't need for user login.

import { z } from "zod";

const GITHUB_AUTHORIZE = "https://github.com/login/oauth/authorize";
const GITHUB_TOKEN = "https://github.com/login/oauth/access_token";
const GITHUB_USER = "https://api.github.com/user";

const GhUserSchema = z.object({
  id: z.number(),
  login: z.string(),
  email: z.string().email().nullable().optional(),
  avatar_url: z.string().url().nullable().optional(),
});

export type GithubUser = z.infer<typeof GhUserSchema>;

export function githubAuthorizeUrl(state: string, redirectUri: string): string {
  const clientId = process.env.GITHUB_OAUTH_CLIENT_ID ?? "";
  const u = new URL(GITHUB_AUTHORIZE);
  u.searchParams.set("client_id", clientId);
  u.searchParams.set("redirect_uri", redirectUri);
  u.searchParams.set("state", state);
  // `read:user` lets us read the user's profile; `user:email` adds email
  // access (private emails included). Both are non-repo scopes.
  u.searchParams.set("scope", "read:user user:email");
  return u.toString();
}

export async function exchangeCodeForToken(
  code: string,
  redirectUri: string,
): Promise<string> {
  const clientId = process.env.GITHUB_OAUTH_CLIENT_ID ?? "";
  const clientSecret = process.env.GITHUB_OAUTH_CLIENT_SECRET ?? "";
  const res = await fetch(GITHUB_TOKEN, {
    method: "POST",
    headers: {
      "Content-Type": "application/x-www-form-urlencoded",
      Accept: "application/json",
    },
    body: new URLSearchParams({
      client_id: clientId,
      client_secret: clientSecret,
      code,
      redirect_uri: redirectUri,
    }),
  });
  if (!res.ok) {
    throw new Error(`github token exchange failed: ${res.status} ${await res.text()}`);
  }
  const json = (await res.json()) as { access_token?: string; error?: string };
  if (json.error) throw new Error(`github oauth error: ${json.error}`);
  if (!json.access_token) throw new Error("github token exchange returned no access_token");
  return json.access_token;
}

export async function fetchGithubUser(accessToken: string): Promise<GithubUser> {
  const res = await fetch(GITHUB_USER, {
    headers: {
      Authorization: `Bearer ${accessToken}`,
      Accept: "application/vnd.github+json",
      "X-GitHub-Api-Version": "2022-11-28",
      "User-Agent": "bolide-website/0.1",
    },
  });
  if (!res.ok) {
    throw new Error(`github /user failed: ${res.status} ${await res.text()}`);
  }
  return GhUserSchema.parse(await res.json());
}
