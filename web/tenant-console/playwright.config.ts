import { existsSync } from "node:fs";

import { defineConfig } from "@playwright/test";

const localEdgePath = "/Applications/Microsoft Edge.app";
const browserChannel = process.env.PLAYWRIGHT_CHANNEL
	? process.env.PLAYWRIGHT_CHANNEL
	: existsSync(localEdgePath)
		? "msedge"
		: undefined;

export default defineConfig({
	testDir: "./tests/e2e",
	use: {
		baseURL: "http://127.0.0.1:4173",
		...(browserChannel ? { channel: browserChannel } : {}),
		headless: true,
	},
	webServer: {
		command:
			"VITE_DEFAULT_TENANT_API_BASE_URL=http://tenant.test " +
			"VITE_DEFAULT_CONTROL_PLANE_BASE_URL=http://control.test " +
			"VITE_DEFAULT_GATEWAY_BASE_URL=http://gateway.test/v1 " +
			"VITE_TENANT_MANAGEMENT_TOKEN=tenant_internal_token " +
			"VITE_CONTROL_PLANE_TOKEN=control_internal_token " +
			"VITE_CONSOLE_SECRET_TOKEN=console_secret " +
			"bun run dev --host 127.0.0.1 --port 4173",
		port: 4173,
		reuseExistingServer: true,
		cwd: ".",
	},
});
