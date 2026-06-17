import type { SessionOptions } from "iron-session";
import { getIronSession } from "iron-session";
import { cookies } from "next/headers";

export type SessionData = {
  userId?: string;
  githubId?: number;
  login?: string;
  // OAuth state is stored in-session for CSRF defense.
  oauthState?: string;
  oauthReturnTo?: string;
};

const password = process.env.SESSION_PASSWORD ?? "";
if (password.length < 32) {
  // Iron-session requires >= 32 chars. Surface this loudly in dev so it's
  // obvious — a 32-byte random hex string is what we want.
  // eslint-disable-next-line no-console
  console.warn(
    "[session] SESSION_PASSWORD is missing or shorter than 32 chars; " +
      "iron-session will reject cookie writes.",
  );
}

export const sessionOptions: SessionOptions = {
  password,
  cookieName: "bolide_session",
  cookieOptions: {
    secure: process.env.NODE_ENV === "production",
    httpOnly: true,
    sameSite: "lax",
    path: "/",
  },
};

export function getSession() {
  return getIronSession<SessionData>(cookies(), sessionOptions);
}
