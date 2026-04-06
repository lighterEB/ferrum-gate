import { describe, expect, it } from "vitest";

import {
	connectSession,
	disconnectSession,
	getSessionSnapshot,
	hydrateSession,
} from "@/session/store";

describe("session store", () => {
	it("connects and disconnects with sanitized service urls", () => {
		connectSession({
			baseUrl: "http://127.0.0.1:3006/",
			token: "tenant-token",
			controlPlaneBaseUrl: "http://127.0.0.1:3007/",
			controlPlaneToken: "control-token",
			gatewayBaseUrl: "http://127.0.0.1:3005/v1/",
		});

		expect(getSessionSnapshot()).toEqual({
			baseUrl: "http://127.0.0.1:3006",
			token: "tenant-token",
			controlPlaneBaseUrl: "http://127.0.0.1:3007",
			controlPlaneToken: "control-token",
			gatewayBaseUrl: "http://127.0.0.1:3005/v1",
			isConnected: true,
			hasControlPlaneAccess: true,
		});

		disconnectSession();

		expect(getSessionSnapshot()).toEqual({
			baseUrl: null,
			token: null,
			controlPlaneBaseUrl: null,
			controlPlaneToken: null,
			gatewayBaseUrl: null,
			isConnected: false,
			hasControlPlaneAccess: false,
		});
	});

	it("hydrates only from session storage", () => {
		window.sessionStorage.setItem("fg.tenant.baseUrl", "http://tenant.test");
		window.sessionStorage.setItem("fg.tenant.token", "tenant-token");
		window.sessionStorage.setItem("fg.control.baseUrl", "http://control.test");
		window.sessionStorage.setItem("fg.control.token", "control-token");
		window.sessionStorage.setItem(
			"fg.gateway.baseUrl",
			"http://gateway.test/v1",
		);

		hydrateSession();

		expect(getSessionSnapshot()).toEqual({
			baseUrl: "http://tenant.test",
			token: "tenant-token",
			controlPlaneBaseUrl: "http://control.test",
			controlPlaneToken: "control-token",
			gatewayBaseUrl: "http://gateway.test/v1",
			isConnected: true,
			hasControlPlaneAccess: true,
		});
	});
});
