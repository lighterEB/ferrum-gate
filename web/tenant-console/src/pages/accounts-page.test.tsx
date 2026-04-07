import { screen, waitFor } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { describe, expect, it } from "vitest";

import { connectSession } from "@/session/store";
import { renderApp } from "@/test/render-app";
import { server } from "@/test/server";

describe("accounts page", () => {
	it("loads provider accounts in proxy/basic-auth mode when control plane base url is empty", async () => {
		let requested = false;

		connectSession({
			baseUrl: "",
			token: "Basic dXNlcjpwYXNz",
			controlPlaneBaseUrl: "",
			controlPlaneToken: "Basic dXNlcjpwYXNz",
			gatewayBaseUrl: "/v1",
		});

		server.use(
			http.get("/internal/v1/runtime/provider-accounts", ({ request }) => {
				requested = true;
				expect(request.headers.get("authorization")).toBe("Basic dXNlcjpwYXNz");

				return HttpResponse.json({
					data: [
						{
							id: "443d867f-a559-4eef-a7a1-901265e9bf86",
							provider: "openai_codex",
							credential_kind: "oauth_tokens",
							payload_version: "v1",
							state: "active",
							external_account_id: "acct_uploaded_123",
							redacted_display: "u***@***",
							plan_type: "plus",
							metadata: { email: "uploaded@example.com" },
							labels: ["uploaded"],
							tags: {},
							capabilities: ["gpt-4.1-mini"],
							expires_at: null,
							last_validated_at: "2026-04-08T00:00:00Z",
							created_at: "2026-04-08T00:00:00Z",
							quota: null,
						},
					],
				});
			}),
		);

		renderApp("/accounts");

		await waitFor(() => {
			expect(requested).toBe(true);
		});
		expect(await screen.findByText("uploaded@example.com")).toBeInTheDocument();
		expect(await screen.findByText("acct_uploaded_123")).toBeInTheDocument();
	});
});
