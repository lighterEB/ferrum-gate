import "@testing-library/jest-dom/vitest";

import { cleanup } from "@testing-library/react";
import { afterAll, afterEach, beforeAll, vi } from "vitest";

import i18n, { LANGUAGE_STORAGE_KEY } from "@/i18n";
import { disconnectSession } from "@/session/store";
import { server } from "@/test/server";

beforeAll(() => {
	server.listen({ onUnhandledRequest: "error" });

	Object.defineProperty(window, "matchMedia", {
		writable: true,
		value: vi.fn().mockImplementation((query: string) => ({
			matches: false,
			media: query,
			onchange: null,
			addEventListener: vi.fn(),
			removeEventListener: vi.fn(),
			addListener: vi.fn(),
			removeListener: vi.fn(),
			dispatchEvent: vi.fn(),
		})),
	});

	window.HTMLElement.prototype.scrollIntoView = vi.fn();
	window.scrollTo = vi.fn();
});

afterEach(() => {
	cleanup();
	server.resetHandlers();
	disconnectSession();
	window.sessionStorage.clear();
	window.localStorage.removeItem(LANGUAGE_STORAGE_KEY);
	void i18n.changeLanguage("zh-CN");
	vi.unstubAllEnvs();
});

afterAll(() => {
	server.close();
});
