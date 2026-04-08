import { describe, expect, it } from "vitest";

import nginxConfig from "../../../../docker/nginx/backend.conf.template?raw";
import nginxDockerfile from "../../../../docker/nginx.Dockerfile?raw";
import readme from "../../../../README.md?raw";
import vpsEnv from "../../../../vps.env.example?raw";

function escapeRegex(value: string) {
	return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function readSection(markdown: string, heading: string) {
	const start = markdown.indexOf(`${heading}\n`);
	if (start === -1) {
		return "";
	}

	const bodyStart = start + heading.length + 1;
	const nextHeading = markdown.indexOf("\n## ", bodyStart);
	return markdown.slice(
		bodyStart,
		nextHeading === -1 ? undefined : nextHeading,
	);
}

describe("public VPS deploy guardrails", () => {
	it("proxies all backend routes through nginx", () => {
		// Backend-only nginx: no SPA, just proxy pass
		for (const route of [
			"/v1/",
			"/tenant/",
			"/internal/",
			"/external/",
			"/health",
		]) {
			expect(nginxConfig).toMatch(
				new RegExp(
					`location\\s+(?:=\\s+)?${escapeRegex(route)}\\s*\\{[\\s\\S]*proxy_pass`,
				),
			);
		}
	});

	it("keeps nginx backend-only without frontend build stage", () => {
		// No frontend builder stage
		expect(nginxDockerfile).not.toMatch(/oven\/bun|frontend-builder/i);
		// No SPA artifact copy
		expect(nginxDockerfile).not.toMatch(/COPY\s+--from=.*dist/i);
		// Still based on nginx
		expect(nginxDockerfile).toMatch(/FROM nginx/i);
	});

	it("documents browser-safe public deployment expectations for tenant-console", () => {
		const tenantConsoleSection = readSection(readme, "## Tenant Console");

		expect(vpsEnv).not.toMatch(
			/VITE_(TENANT_MANAGEMENT_TOKEN|CONTROL_PLANE_TOKEN|CONSOLE_SECRET_TOKEN|CONSOLE_USERNAME|CONSOLE_PASSWORD)/,
		);
		expect(tenantConsoleSection).toMatch(
			/local flow|development|public VPS deployment/i,
		);
		expect(tenantConsoleSection).toMatch(
			/public deployment|public VPS deployment|browser/i,
		);
		expect(tenantConsoleSection).toMatch(/must not|do\s*\*\*?not/i);
		expect(tenantConsoleSection).toMatch(/secret/i);
	});
});
