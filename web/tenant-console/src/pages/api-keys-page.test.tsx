import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { HttpResponse, http } from "msw";
import { describe, expect, it } from "vitest";

import i18n from "@/i18n";
import { connectSession } from "@/session/store";
import { server } from "@/test/server";

const tenantMe = {
	id: "tenant_1",
	slug: "demo",
	name: "Demo Tenant",
	suspended: false,
	created_at: "2026-04-05T00:00:00Z",
};

function registerWorkspaceHandlers(
	apiKeys: Array<{
		id: string;
		tenant_id: string;
		label: string;
		prefix: string;
		status: "active" | "revoked";
		created_at: string;
		last_used_at: string | null;
	}>,
) {
	server.use(
		http.get("http://tenant.test/tenant/v1/me", () =>
			HttpResponse.json(tenantMe),
		),
		http.get("http://tenant.test/tenant/v1/models", () =>
			HttpResponse.json({ data: [] }),
		),
		http.get("http://tenant.test:3005/health", () =>
			HttpResponse.json({ status: "ok" }),
		),
		http.get("http://tenant.test/tenant/v1/api-keys", () =>
			HttpResponse.json({ data: apiKeys }),
		),
	);
}

describe("api keys page", () => {
	it("loads and creates api keys", async () => {
		const user = userEvent.setup();
		let apiKeys = [
			{
				id: "key_1",
				tenant_id: "tenant_1",
				label: "SDK",
				prefix: "fgk_key_1",
				status: "active" as const,
				created_at: "2026-04-05T00:00:00Z",
				last_used_at: null,
			},
		];

		connectSession({
			baseUrl: "http://tenant.test",
			token: "fg_token",
		});

		registerWorkspaceHandlers(apiKeys);
		server.use(
			http.post(
				"http://tenant.test/tenant/v1/api-keys",
				async ({ request }) => {
					const payload = (await request.json()) as { label: string };
					const created = {
						record: {
							id: "key_2",
							tenant_id: "tenant_1",
							label: payload.label,
							prefix: "fgk_key_2",
							status: "active" as const,
							created_at: "2026-04-05T01:00:00Z",
							last_used_at: null,
						},
						secret: "fgk_created_secret",
					};
					apiKeys = [...apiKeys, created.record];
					return HttpResponse.json(created);
				},
			),
		);

		const { renderApp } = await import("@/test/render-app");
		renderApp("/api-keys");

		expect(await screen.findByText("SDK")).toBeInTheDocument();

		await user.type(
			screen.getByLabelText(i18n.t("apiKeys.label")),
			"Cherry Studio",
		);
		await user.click(
			screen.getByRole("button", {
				name: i18n.t("apiKeys.createSubmit"),
			}),
		);

		expect(
			await screen.findByText(i18n.t("secretCard.title")),
		).toBeInTheDocument();
		expect(screen.getByText("fgk_created_secret")).toBeInTheDocument();
		expect(await screen.findAllByText("Cherry Studio")).not.toHaveLength(0);
	});

	it("rotates an api key and reveals the new secret", async () => {
		const user = userEvent.setup();
		let apiKeys = [
			{
				id: "key_1",
				tenant_id: "tenant_1",
				label: "SDK",
				prefix: "fgk_key_1",
				status: "active" as const,
				created_at: "2026-04-05T00:00:00Z",
				last_used_at: null,
			},
		];

		connectSession({
			baseUrl: "http://tenant.test",
			token: "fg_token",
		});

		registerWorkspaceHandlers(apiKeys);
		server.use(
			http.post("http://tenant.test/tenant/v1/api-keys/key_1/rotate", () => {
				const created = {
					record: {
						...apiKeys[0],
						prefix: "fgk_key_1_rotated",
					},
					secret: "fgk_rotated_secret",
				};
				apiKeys = [created.record];
				return HttpResponse.json(created);
			}),
		);

		const { renderApp } = await import("@/test/render-app");
		renderApp("/api-keys");

		expect(await screen.findByText("SDK")).toBeInTheDocument();

		await user.click(
			screen.getByRole("button", { name: i18n.t("common.rotate") }),
		);

		expect(
			await screen.findByText(i18n.t("secretCard.title")),
		).toBeInTheDocument();
		expect(screen.getByText("fgk_rotated_secret")).toBeInTheDocument();
	});

	it("revokes an api key and updates the card status", async () => {
		const user = userEvent.setup();
		let apiKeys: Array<{
			id: string;
			tenant_id: string;
			label: string;
			prefix: string;
			status: "active" | "revoked";
			created_at: string;
			last_used_at: string | null;
		}> = [
			{
				id: "key_1",
				tenant_id: "tenant_1",
				label: "SDK",
				prefix: "fgk_key_1",
				status: "active" as const,
				created_at: "2026-04-05T00:00:00Z",
				last_used_at: null,
			},
		];

		connectSession({
			baseUrl: "http://tenant.test",
			token: "fg_token",
		});

		registerWorkspaceHandlers(apiKeys);
		server.use(
			http.post("http://tenant.test/tenant/v1/api-keys/key_1/revoke", () => {
				const revoked = {
					...apiKeys[0],
					status: "revoked" as const,
				};
				apiKeys = [revoked];
				return HttpResponse.json(revoked);
			}),
		);

		const { renderApp } = await import("@/test/render-app");
		renderApp("/api-keys");

		expect(await screen.findByText("SDK")).toBeInTheDocument();

		await user.click(
			screen.getByRole("button", { name: i18n.t("common.revoke") }),
		);

		await waitFor(() => {
			expect(
				screen.getByText(i18n.t("apiKeys.toast.revoked")),
			).toBeInTheDocument();
		});
	});
});
