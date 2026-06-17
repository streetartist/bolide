import { prisma } from "./prisma";
import { getSession } from "./session";

export class HttpError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

export async function getCurrentUser() {
  const session = await getSession();
  if (!session.userId) return null;
  return prisma.user.findUnique({ where: { id: session.userId } });
}

export async function requireUser() {
  const user = await getCurrentUser();
  if (!user) throw new HttpError(401, "Login required");
  return user;
}

export async function requireAdmin() {
  const user = await requireUser();
  if (user.role !== "admin") throw new HttpError(403, "Admin only");
  return user;
}
