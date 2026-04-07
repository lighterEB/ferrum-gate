import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync, rmSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = path.resolve(
	path.dirname(fileURLToPath(import.meta.url)),
	"..",
);
const distRoot = path.join(projectRoot, "dist");
const repoRoot = path.resolve(projectRoot, "..", "..");

const forbiddenBuildValues = [
	"tenant_internal_token",
	"control_internal_token",
	"console_secret",
	"ops-admin",
	"ops-password",
];

function walkFiles(currentPath) {
	if (statSync(currentPath).isDirectory()) {
		return readdirSync(currentPath).flatMap((entry) =>
			walkFiles(path.join(currentPath, entry)),
		);
	}

	return [currentPath];
}

rmSync(distRoot, { recursive: true, force: true });

execFileSync("bun", ["run", "build"], {
	cwd: projectRoot,
	stdio: "inherit",
	env: {
		...process.env,
		VITE_DEFAULT_TENANT_API_BASE_URL: "https://console.example.com/tenant",
		VITE_DEFAULT_CONTROL_PLANE_BASE_URL: "https://console.example.com/internal",
		VITE_DEFAULT_GATEWAY_BASE_URL: "https://console.example.com/v1",
		VITE_TENANT_MANAGEMENT_TOKEN: "tenant_internal_token",
		VITE_CONTROL_PLANE_TOKEN: "control_internal_token",
		VITE_CONSOLE_SECRET_TOKEN: "console_secret",
		VITE_CONSOLE_USERNAME: "ops-admin",
		VITE_CONSOLE_PASSWORD: "ops-password",
	},
});

const distFiles = walkFiles(distRoot).filter((filePath) =>
	/\.(html|js|css|json|txt|map)$/i.test(filePath),
);

const matches = [];

for (const filePath of distFiles) {
	const contents = readFileSync(filePath, "utf8");

	for (const forbiddenValue of forbiddenBuildValues) {
		if (contents.includes(forbiddenValue)) {
			matches.push({
				file: path.relative(repoRoot, filePath),
				forbiddenValue,
			});
		}
	}
}

if (matches.length > 0) {
	console.error("Public build still ships browser-visible admin secrets:");
	for (const match of matches) {
		console.error(`- ${match.file}: ${match.forbiddenValue}`);
	}
	process.exit(1);
}

console.log(
	"PASS: tenant-console production build does not embed browser-visible admin secrets.",
);
