import { screen, waitFor } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { describe, expect, it } from "vitest";

import i18n from "@/i18n";
import { connectSession } from "@/session/store";
import { renderApp } from "@/test/render-app";
import { server } from "@/test/server";

describe("router guards", () => {
	it("redirects disconnected users from /api-keys to /connect", async () => {
		const { router } = renderApp("/api-keys");

		await waitFor(() => {
			expect(router.state.location.pathname).toBe("/connect");
		});
	});

	it("redirects connected users from /connect to /api-keys", async () => {
		connectSession({
			baseUrl: "http://tenant.test",
			token: "fg_token",
		});

		server.use(
			http.get("http://tenant.test/tenant/v1/me", () =>
				HttpResponse.json({
					id: "tenant_1",
					slug: "demo",
					name: "Demo Tenant",
					suspended: false,
					created_at: "2026-04-05T00:00:00Z",
				}),
			),
			http.get("http://tenant.test/tenant/v1/models", () =>
				HttpResponse.json({ data: [] }),
			),
			http.get("http://tenant.test/tenant/v1/api-keys", () =>
				HttpResponse.json({ data: [] }),
			),
			http.get("http://tenant.test:3005/health", () =>
				HttpResponse.json({ status: "ok" }),
			),
		);

		const { router } = renderApp("/connect");

		await waitFor(() => {
			expect(router.state.location.pathname).toBe("/dashboard");
		});
	});

	it("disconnects and returns the user to /connect", async () => {
		connectSession({
			baseUrl: "http://tenant.test",
			token: "fg_token",
		});

		server.use(
			http.get("http://tenant.test/tenant/v1/me", () =>
				HttpResponse.json({
					id: "tenant_1",
					slug: "demo",
					name: "Demo Tenant",
					suspended: false,
					created_at: "2026-04-05T00:00:00Z",
				}),
			),
			http.get("http://tenant.test/tenant/v1/models", () =>
				HttpResponse.json({ data: [] }),
			),
			http.get("http://tenant.test/tenant/v1/api-keys", () =>
				HttpResponse.json({ data: [] }),
			),
			http.get("http://tenant.test:3005/health", () =>
				HttpResponse.json({ status: "ok" }),
			),
		);

		const { router } = renderApp("/dashboard");
		await screen.findByRole("button", { name: i18n.t("common.disconnect") });

		screen.getByRole("button", { name: i18n.t("common.disconnect") }).click();

		await waitFor(() => {
			expect(router.state.location.pathname).toBe("/connect");
		});
	});
});
