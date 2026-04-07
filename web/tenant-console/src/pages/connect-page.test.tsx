import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { describe, expect, it, vi } from "vitest";

import i18n from "@/i18n";
import { getSessionSnapshot } from "@/session/store";
import { renderApp } from "@/test/render-app";
import { server } from "@/test/server";

describe("connect page", () => {
	it("verifies the console secret token and navigates to the workspace", async () => {
		const user = userEvent.setup();
		vi.stubEnv("VITE_DEFAULT_TENANT_API_BASE_URL", "http://tenant.test");
		vi.stubEnv("VITE_DEFAULT_CONTROL_PLANE_BASE_URL", "http://control.test");
		vi.stubEnv("VITE_DEFAULT_GATEWAY_BASE_URL", "http://gateway.test/v1");
		vi.stubEnv("VITE_TENANT_MANAGEMENT_TOKEN", "tenant_internal_token");
		vi.stubEnv("VITE_CONTROL_PLANE_TOKEN", "control_internal_token");
		vi.stubEnv("VITE_CONSOLE_SECRET_TOKEN", "console_secret");

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
			http.get(
				"http://control.test/internal/v1/runtime/provider-accounts",
				() => HttpResponse.json({ data: [] }),
			),
			http.get("http://control.test/internal/v1/alerts/outbox", () =>
				HttpResponse.json({ data: [] }),
			),
			http.get("http://control.test/internal/v1/audit/events", () =>
				HttpResponse.json({ data: [] }),
			),
			http.get("http://tenant.test/tenant/v1/api-keys", () =>
				HttpResponse.json({ data: [] }),
			),
			http.get("http://tenant.test/tenant/v1/models", () =>
				HttpResponse.json({ data: [] }),
			),
			http.get("http://gateway.test/health", () =>
				HttpResponse.json({ status: "ok" }),
			),
		);

		const { router } = renderApp("/connect");
		const connectButton = await screen.findByRole("button", {
			name: i18n.t("common.connect"),
		});
		await screen.findByLabelText(i18n.t("connect.fields.secretToken.label"));

		await user.type(
			screen.getByLabelText(i18n.t("connect.fields.secretToken.label")),
			"console_secret",
		);
		await user.click(connectButton);

		await waitFor(() => {
			expect(router.state.location.pathname).toBe("/dashboard");
		});

		expect(
			await screen.findByRole("heading", { name: i18n.t("dashboard.title") }),
		).toBeInTheDocument();
		expect(getSessionSnapshot().baseUrl).toBe("http://tenant.test");
		expect(getSessionSnapshot().controlPlaneBaseUrl).toBe(
			"http://control.test",
		);
		expect(getSessionSnapshot().gatewayBaseUrl).toBe("http://gateway.test/v1");
	});

	it("shows an authentication error and stays on connect when the console secret is rejected", async () => {
		const user = userEvent.setup();
		vi.stubEnv("VITE_DEFAULT_TENANT_API_BASE_URL", "http://tenant.test");
		vi.stubEnv("VITE_DEFAULT_CONTROL_PLANE_BASE_URL", "http://control.test");
		vi.stubEnv("VITE_DEFAULT_GATEWAY_BASE_URL", "http://gateway.test/v1");
		vi.stubEnv("VITE_TENANT_MANAGEMENT_TOKEN", "tenant_internal_token");
		vi.stubEnv("VITE_CONTROL_PLANE_TOKEN", "control_internal_token");
		vi.stubEnv("VITE_CONSOLE_SECRET_TOKEN", "console_secret");

		const { router } = renderApp("/connect");
		const connectButton = await screen.findByRole("button", {
			name: i18n.t("common.connect"),
		});
		await screen.findByLabelText(i18n.t("connect.fields.secretToken.label"));

		await user.type(
			screen.getByLabelText(i18n.t("connect.fields.secretToken.label")),
			"bad-token",
		);
		await user.click(connectButton);

		expect(
			await screen.findByText(
				i18n.t("connect.errors.invalidConsoleCredentials"),
			),
		).toBeInTheDocument();
		expect(router.state.location.pathname).toBe("/connect");
		expect(getSessionSnapshot().isConnected).toBe(false);
	});

	it("supports username and password login when configured", async () => {
		const user = userEvent.setup();
		vi.stubEnv("VITE_DEFAULT_TENANT_API_BASE_URL", "http://tenant.test");
		vi.stubEnv("VITE_DEFAULT_CONTROL_PLANE_BASE_URL", "http://control.test");
		vi.stubEnv("VITE_DEFAULT_GATEWAY_BASE_URL", "http://gateway.test/v1");
		vi.stubEnv("VITE_TENANT_MANAGEMENT_TOKEN", "tenant_internal_token");
		vi.stubEnv("VITE_CONTROL_PLANE_TOKEN", "control_internal_token");
		vi.stubEnv("VITE_CONSOLE_SECRET_TOKEN", "console_secret");
		vi.stubEnv("VITE_CONSOLE_USERNAME", "ops-admin");
		vi.stubEnv("VITE_CONSOLE_PASSWORD", "ops-password");

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
			http.get(
				"http://control.test/internal/v1/runtime/provider-accounts",
				() => HttpResponse.json({ data: [] }),
			),
			http.get("http://control.test/internal/v1/alerts/outbox", () =>
				HttpResponse.json({ data: [] }),
			),
			http.get("http://control.test/internal/v1/audit/events", () =>
				HttpResponse.json({ data: [] }),
			),
			http.get("http://tenant.test/tenant/v1/api-keys", () =>
				HttpResponse.json({ data: [] }),
			),
			http.get("http://tenant.test/tenant/v1/models", () =>
				HttpResponse.json({ data: [] }),
			),
			http.get("http://gateway.test/health", () =>
				HttpResponse.json({ status: "ok" }),
			),
		);

		const { router } = renderApp("/connect");
		await screen.findByLabelText(i18n.t("connect.fields.secretToken.label"));

		await user.click(
			screen.getByRole("button", { name: i18n.t("connect.modes.password") }),
		);
		await user.type(
			screen.getByLabelText(i18n.t("connect.fields.username.label")),
			"ops-admin",
		);
		await user.type(
			screen.getByLabelText(i18n.t("connect.fields.password.label")),
			"ops-password",
		);
		await user.click(
			screen.getByRole("button", { name: i18n.t("common.connect") }),
		);

		await waitFor(() => {
			expect(router.state.location.pathname).toBe("/dashboard");
		});
	});

	it("connects through same-origin proxy auth without browser-visible secrets", async () => {
		const user = userEvent.setup();
		vi.stubEnv("DEV", false);
		vi.stubEnv("VITE_DEFAULT_TENANT_API_BASE_URL", "");
		vi.stubEnv("VITE_DEFAULT_CONTROL_PLANE_BASE_URL", "");
		vi.stubEnv("VITE_DEFAULT_GATEWAY_BASE_URL", "/v1");
		vi.stubEnv("VITE_TENANT_MANAGEMENT_TOKEN", "");
		vi.stubEnv("VITE_CONTROL_PLANE_TOKEN", "");
		vi.stubEnv("VITE_CONSOLE_SECRET_TOKEN", "");
		vi.stubEnv("VITE_CONSOLE_USERNAME", "");
		vi.stubEnv("VITE_CONSOLE_PASSWORD", "");

		server.use(
			http.get("/tenant/v1/me", ({ request }) => {
				expect(request.headers.get("authorization")).toBeNull();

				return HttpResponse.json({
					id: "tenant_1",
					slug: "demo",
					name: "Demo Tenant",
					suspended: false,
					created_at: "2026-04-05T00:00:00Z",
				});
			}),
			http.get("/internal/v1/runtime/provider-accounts", ({ request }) => {
				expect(request.headers.get("authorization")).toBeNull();
				return HttpResponse.json({ data: [] });
			}),
			http.get("/internal/v1/alerts/outbox", () =>
				HttpResponse.json({ data: [] }),
			),
			http.get("/internal/v1/audit/events", () =>
				HttpResponse.json({ data: [] }),
			),
			http.get("/tenant/v1/api-keys", () => HttpResponse.json({ data: [] })),
			http.get("/tenant/v1/models", () => HttpResponse.json({ data: [] })),
			http.get("/health", () => HttpResponse.json({ status: "ok" })),
		);

		const { router } = renderApp("/connect");
		const connectButton = await screen.findByRole("button", {
			name: i18n.t("common.connect"),
		});

		expect(
			screen.queryByLabelText(i18n.t("connect.fields.secretToken.label")),
		).not.toBeInTheDocument();
		expect(
			screen.queryByLabelText(i18n.t("connect.fields.username.label")),
		).not.toBeInTheDocument();

		await user.click(connectButton);

		await waitFor(() => {
			expect(router.state.location.pathname).toBe("/dashboard");
		});

		expect(getSessionSnapshot().baseUrl).toBe("");
		expect(getSessionSnapshot().controlPlaneBaseUrl).toBeNull();
		expect(getSessionSnapshot().gatewayBaseUrl).toBe("/v1");
	});
});
