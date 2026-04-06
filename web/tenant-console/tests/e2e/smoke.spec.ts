import { expect, test } from "@playwright/test";

test("connect page renders in chinese by default", async ({ page }) => {
	await page.goto("/connect");
	await expect(
		page.getByRole("heading", { name: "连接 FerrumGate 控制台", level: 1 }),
	).toBeVisible();
});

test("connects and creates an api key", async ({ page }) => {
	await page.route("http://tenant.test/tenant/v1/me", async (route) => {
		await route.fulfill({
			status: 200,
			contentType: "application/json",
			body: JSON.stringify({
				id: "tenant_1",
				slug: "demo",
				name: "Demo Tenant",
				suspended: false,
				created_at: "2026-04-05T00:00:00Z",
			}),
		});
	});

	await page.route("http://tenant.test/tenant/v1/api-keys", async (route) => {
		if (route.request().method() === "GET") {
			await route.fulfill({
				status: 200,
				contentType: "application/json",
				body: JSON.stringify({ data: [] }),
			});
			return;
		}

		await route.fulfill({
			status: 200,
			contentType: "application/json",
			body: JSON.stringify({
				record: {
					id: "key_1",
					tenant_id: "tenant_1",
					label: "SDK",
					prefix: "fgk_key_1",
					status: "active",
					created_at: "2026-04-05T00:00:00Z",
					last_used_at: null,
				},
				secret: "fgk_created_secret",
			}),
		});
	});
	await page.route("http://tenant.test/tenant/v1/models", async (route) => {
		await route.fulfill({
			status: 200,
			contentType: "application/json",
			body: JSON.stringify({ data: [] }),
		});
	});

	await page.route(
		"http://control.test/internal/v1/runtime/provider-accounts",
		async (route) => {
			await route.fulfill({
				status: 200,
				contentType: "application/json",
				body: JSON.stringify({ data: [] }),
			});
		},
	);
	await page.route(
		"http://control.test/internal/v1/alerts/outbox",
		async (route) => {
			await route.fulfill({
				status: 200,
				contentType: "application/json",
				body: JSON.stringify({ data: [] }),
			});
		},
	);
	await page.route(
		"http://control.test/internal/v1/audit/events",
		async (route) => {
			await route.fulfill({
				status: 200,
				contentType: "application/json",
				body: JSON.stringify({ data: [] }),
			});
		},
	);
	await page.route("http://gateway.test/health", async (route) => {
		await route.fulfill({
			status: 200,
			contentType: "application/json",
			body: JSON.stringify({ status: "ok" }),
		});
	});

	await page.goto("/connect");
	await page.getByLabel("运营台 Secret Token").fill("console_secret");
	await page.getByRole("button", { name: "连接" }).click();

	await expect(
		page.getByRole("heading", { name: "总览", level: 1 }),
	).toBeVisible();
	await page
		.getByRole("navigation")
		.getByRole("link", { name: "API Key" })
		.click();
	await page.getByLabel("Label").fill("SDK");
	await page.getByRole("button", { name: "创建并显示 Secret" }).click();

	await expect(
		page.locator('[data-slot="card-title"]', {
			hasText: "一次性 Secret",
		}),
	).toBeVisible();
	await expect(page.getByText("fgk_created_secret")).toBeVisible();
});
