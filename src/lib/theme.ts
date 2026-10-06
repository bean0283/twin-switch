import { readStored, removeStored, STORAGE_PREFIX, writeStored } from "@/lib/storage-keys";

export type ThemePreference = "system" | "light" | "dark";

/** 主题偏好。**读取一律走 `storage-keys`**，它会自动回退读改名前的旧键。 */
const THEME_STORAGE_KEY = `${STORAGE_PREFIX}.theme`;
/** 更早的一版：只存「是否深色」（`1` / `0`）。仅用于回退读取，不再写入。 */
const LEGACY_DARK_STORAGE_KEY = `${STORAGE_PREFIX}.dark`;

function isThemePreference(value: string | null): value is ThemePreference {
  return value === "system" || value === "light" || value === "dark";
}

export function getThemePreference(): ThemePreference {
  const stored = readStored(THEME_STORAGE_KEY);
  if (isThemePreference(stored)) return stored;

  const legacyDark = readStored(LEGACY_DARK_STORAGE_KEY);
  if (legacyDark === "1") return "dark";
  if (legacyDark === "0") return "light";

  return "system";
}

export function applyTheme(preference: ThemePreference) {
  const dark =
    preference === "dark" ||
    (preference === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.classList.toggle("dark", dark);
}

export function setThemePreference(preference: ThemePreference) {
  writeStored(THEME_STORAGE_KEY, preference);
  // 旧的那版开关是「是否深色」，与新的三态语义不同，留着会在读回退时造成歧义。
  removeStored(LEGACY_DARK_STORAGE_KEY);
  applyTheme(preference);
}

export function watchSystemTheme() {
  const media = window.matchMedia("(prefers-color-scheme: dark)");
  const onChange = () => {
    if (getThemePreference() === "system") applyTheme("system");
  };

  media.addEventListener("change", onChange);
  return () => media.removeEventListener("change", onChange);
}
