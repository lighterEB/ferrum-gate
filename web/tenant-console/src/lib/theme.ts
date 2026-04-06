import { useSyncExternalStore } from "react";

export const THEME_STORAGE_KEY = "fg.console.theme";

export type Theme = "dark" | "light";

const listeners = new Set<() => void>();

let currentTheme: Theme = "dark";

function canUseDom() {
	return (
		typeof window !== "undefined" &&
		typeof document !== "undefined" &&
		typeof window.localStorage !== "undefined"
	);
}

function readStoredTheme(): Theme {
	if (!canUseDom()) {
		return currentTheme;
	}

	return window.localStorage.getItem(THEME_STORAGE_KEY) === "light"
		? "light"
		: "dark";
}

function applyTheme(theme: Theme) {
	if (!canUseDom()) {
		return;
	}

	const root = document.documentElement;
	root.classList.toggle("light", theme === "light");
	root.style.colorScheme = theme;
}

function emitChange() {
	for (const listener of listeners) {
		listener();
	}
}

export function getStoredTheme(): Theme {
	currentTheme = readStoredTheme();
	return currentTheme;
}

export function setTheme(theme: Theme) {
	currentTheme = theme;

	if (canUseDom()) {
		window.localStorage.setItem(THEME_STORAGE_KEY, theme);
	}

	applyTheme(theme);
	emitChange();
}

export function useTheme() {
	const theme = useSyncExternalStore(
		(listener) => {
			listeners.add(listener);
			return () => listeners.delete(listener);
		},
		() => currentTheme,
		() => currentTheme,
	);

	return {
		theme,
		setTheme,
		toggleTheme() {
			setTheme(theme === "dark" ? "light" : "dark");
		},
	};
}

currentTheme = readStoredTheme();
applyTheme(currentTheme);
