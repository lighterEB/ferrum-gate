import { describe, expect, it } from "vitest";

import nginxConfig from "../../../../docker/nginx/backend.conf?raw";
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
	it("serves the tenant console shell through nginx with SPA fallback", () => {
		expect(nginxConfig).toMatch(
			/try_files\s+\$uri(?:\s+\$uri\/)?\s+\/index\.html\s*;/,
		);

		for (const route of ["/v1/", "/tenant/", "/internal/"]) {
			expect(nginxConfig).toMatch(
				new RegExp(
					`location\\s+${escapeRegex(route)}\\s*\\{[\\s\\S]*proxy_pass`,
				),
			);
		}
	});

	it("wires frontend build artifacts into the nginx runtime image", () => {
		expect(nginxDockerfile).toMatch(/\/usr\/share\/nginx\/html/);
		expect(nginxDockerfile).toMatch(
			/COPY\s+.*index\.html|COPY\s+--from=.*dist/i,
		);
	});

	it("documents browser-safe public deployment expectations for tenant-console", () => {
		const tenantConsoleSection = readSection(readme, "## Tenant Console");

		expect(vpsEnv).not.toMatch(
			/VITE_(TENANT_MANAGEMENT_TOKEN|CONTROL_PLANE_TOKEN|CONSOLE_SECRET_TOKEN|CONSOLE_USERNAME|CONSOLE_PASSWORD)/,
		);
		expect(tenantConsoleSection).toMatch(/dev-only/i);
		expect(tenantConsoleSection).toMatch(
			/public deployment|public VPS deployment|browser/i,
		);
		expect(tenantConsoleSection).toMatch(/must not|do not/i);
		expect(tenantConsoleSection).toMatch(/secret/i);
	});
});
