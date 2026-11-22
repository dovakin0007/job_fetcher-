import { useEffect, useState } from "react";

export type Theme = "light" | "dark";

function currentTheme(): Theme {
  return document.documentElement.dataset.theme === "dark" ? "dark" : "light";
}

export function useTheme() {
  const [theme, setTheme] = useState<Theme>(currentTheme);

  useEffect(() => {
    const sync = () => setTheme(currentTheme());
    window.addEventListener("theme-change", sync);
    return () => window.removeEventListener("theme-change", sync);
  }, []);

  function toggle() {
    const next: Theme = currentTheme() === "dark" ? "light" : "dark";
    document.documentElement.dataset.theme = next;
    localStorage.setItem("theme", next);
    window.dispatchEvent(new Event("theme-change"));
  }

  return { theme, toggle };
}

export function ThemeButton() {
  const { theme, toggle } = useTheme();
  return (
    <button type="button" className="btn secondary" onClick={toggle} aria-pressed={theme === "dark"}>
      {theme === "dark" ? "Light" : "Dark"}
    </button>
  );
}
