import { describe, expect, it } from "vitest";

import {
	getConfiguredControlPlaneToken,
	getConfiguredTenantManagementToken,
	getConsolePassword,
	getConsoleSecretToken,
	getConsoleUsername,
	getDefaultTenantApiBaseUrl,
	isDevRuntime,
} from "@/lib/env";

describe("env helpers", () => {
	it("treats only true-like DEV flags as development", () => {
		expect(isDevRuntime({ DEV: true })).toBe(true);
		expect(isDevRuntime({ DEV: "true" })).toBe(true);
		expect(isDevRuntime({ DEV: false })).toBe(false);
		expect(isDevRuntime({ DEV: "false" })).toBe(false);
		expect(isDevRuntime({})).toBe(false);
	});

	it("uses configured service tokens before falling back to dev defaults", () => {
		expect(
			getConfiguredTenantManagementToken({
				VITE_TENANT_MANAGEMENT_TOKEN: "tenant_internal_token",
			}),
		).toBe("tenant_internal_token");

		expect(
			getConfiguredControlPlaneToken({
				VITE_CONTROL_PLANE_TOKEN: "control_internal_token",
			}),
		).toBe("control_internal_token");

		expect(
			getConfiguredTenantManagementToken({
				DEV: true,
				VITE_DEV_TOKEN: "fg_tenant_admin_demo",
			}),
		).toBe("fg_tenant_admin_demo");

		expect(
			getConfiguredTenantManagementToken({
				DEV: false,
				VITE_DEV_TOKEN: "fg_tenant_admin_demo",
			}),
		).toBe("");
	});

	it("returns console login credentials exactly as configured", () => {
		expect(
			getConsoleSecretToken({
				VITE_CONSOLE_SECRET_TOKEN: "console_secret",
			}),
		).toBe("console_secret");
		expect(
			getConsoleUsername({
				VITE_CONSOLE_USERNAME: "ops-admin",
			}),
		).toBe("ops-admin");
		expect(
			getConsolePassword({
				VITE_CONSOLE_PASSWORD: "ops-password",
			}),
		).toBe("ops-password");
	});

	it("returns the configured tenant api base url when present", () => {
		expect(
			getDefaultTenantApiBaseUrl({
				VITE_DEFAULT_TENANT_API_BASE_URL: "http://127.0.0.1:3006 ",
			}),
		).toBe("http://127.0.0.1:3006");
	});
});
