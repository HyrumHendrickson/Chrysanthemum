// The local identity shown to collaborators (name and cursor colour).

export interface User {
  name: string;
  color: string;
  colorLight: string;
}

const PALETTE: [string, string][] = [
  ["#2f5da8", "#2f5da833"],
  ["#b3412f", "#b3412f33"],
  ["#2b7a63", "#2b7a6333"],
  ["#8a4fa3", "#8a4fa333"],
  ["#a8741a", "#a8741a33"],
  ["#1f7a8c", "#1f7a8c33"],
  ["#c0507a", "#c0507a33"],
  ["#5b6a82", "#5b6a8233"],
];

const KEY = "chrysanthemum.user";

export function loadUser(): User {
  try {
    const raw = localStorage.getItem(KEY);
    if (raw) {
      const u = JSON.parse(raw) as Partial<User>;
      if (typeof u.name === "string" && typeof u.color === "string") {
        return { name: u.name, color: u.color, colorLight: u.colorLight ?? `${u.color}33` };
      }
    }
  } catch {
    /* fall through to a fresh identity */
  }
  const [color, colorLight] = PALETTE[Math.floor(Math.random() * PALETTE.length)] as [string, string];
  const user = { name: `Guest ${Math.floor(100 + Math.random() * 900)}`, color, colorLight };
  saveUser(user);
  return user;
}

export function saveUser(user: User) {
  try {
    localStorage.setItem(KEY, JSON.stringify(user));
  } catch {
    /* storage unavailable; identity lasts for this session only */
  }
}

export function initials(name: string): string {
  const parts = name.trim().split(/\s+/).filter(Boolean);
  if (parts.length === 0) return "?";
  const first = parts[0]?.[0] ?? "?";
  const second = parts.length > 1 ? (parts[parts.length - 1]?.[0] ?? "") : "";
  return (first + second).toUpperCase();
}

export const colorChoices = PALETTE;
