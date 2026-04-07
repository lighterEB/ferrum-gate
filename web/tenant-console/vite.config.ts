import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { loadEnv } from "vite";
import { configDefaults, defineConfig } from "vitest/config";

// https://vite.dev/config/
export default defineConfig(({ mode }) => {
	const envDir = path.resolve(__dirname, "../..");
	const env = loadEnv(mode, envDir, "");

	return {
		envDir,
		plugins: [react(), tailwindcss()],
		resolve: {
			alias: {
				"@": path.resolve(__dirname, "./src"),
			},
		},
		server: {
			proxy: {
				"/tenant": {
					target: env.VITE_PROXY_TENANT_API_TARGET || "http://127.0.0.1:3006",
					changeOrigin: true,
					headers: env.VITE_DEV_TOKEN
						? { Authorization: `Bearer ${env.VITE_DEV_TOKEN}` }
						: undefined,
				},
				"/internal": {
					target:
						env.VITE_PROXY_CONTROL_PLANE_TARGET || "http://127.0.0.1:3007",
					changeOrigin: true,
					headers: env.VITE_DEV_CONTROL_PLANE_TOKEN
						? { Authorization: `Bearer ${env.VITE_DEV_CONTROL_PLANE_TOKEN}` }
						: undefined,
				},
				"/v1": {
					target: env.VITE_PROXY_GATEWAY_TARGET || "http://127.0.0.1:3005",
					changeOrigin: true,
				},
				"/health": {
					target: env.VITE_PROXY_GATEWAY_TARGET || "http://127.0.0.1:3005",
					changeOrigin: true,
				},
			},
		},
		test: {
			globals: true,
			environment: "jsdom",
			setupFiles: "./src/test/setup.ts",
			css: true,
			exclude: [...configDefaults.exclude, "tests/e2e/**", "**/tests/e2e/**"],
		},
	};
});
