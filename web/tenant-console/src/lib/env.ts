type RuntimeEnv = {
	DEV?: boolean | string;
	VITE_DEFAULT_TENANT_API_BASE_URL?: string;
	VITE_DEFAULT_CONTROL_PLANE_BASE_URL?: string;
	VITE_DEFAULT_GATEWAY_BASE_URL?: string;
	VITE_TENANT_MANAGEMENT_TOKEN?: string;
	VITE_CONTROL_PLANE_TOKEN?: string;
	VITE_CONSOLE_SECRET_TOKEN?: string;
	VITE_CONSOLE_USERNAME?: string;
	VITE_CONSOLE_PASSWORD?: string;
	VITE_DEV_TOKEN?: string;
	VITE_DEV_CONTROL_PLANE_TOKEN?: string;
};

function isLoopbackHost(hostname: string) {
	return (
		hostname === "127.0.0.1" || hostname === "localhost" || hostname === "::1"
	);
}

function adaptUrlToBrowserHost(urlText: string) {
	if (!urlText || typeof window === "undefined") {
		return urlText;
	}

	try {
		const url = new URL(urlText);
		const browserHost = window.location.hostname;
		if (!browserHost) {
			return urlText;
		}

		if (isLoopbackHost(url.hostname) && !isLoopbackHost(browserHost)) {
			url.hostname = browserHost;
			return url.toString().replace(/\/+$/, "");
		}

		return url.toString().replace(/\/+$/, "");
	} catch {
		return urlText;
	}
}

export function isDevRuntime(env: RuntimeEnv = import.meta.env) {
	return env.DEV === true || env.DEV === "true";
}

export function getDefaultTenantApiBaseUrl(env: RuntimeEnv = import.meta.env) {
	if (!isDevRuntime(env) && !env.VITE_DEFAULT_TENANT_API_BASE_URL?.trim()) {
		return "";
	}

	return adaptUrlToBrowserHost(
		env.VITE_DEFAULT_TENANT_API_BASE_URL?.trim() ?? "",
	);
}

function deriveSiblingServiceUrl(baseUrl: string, port: string, suffix = "") {
	if (!baseUrl) {
		return "";
	}

	try {
		const url = new URL(baseUrl);
		url.port = port;
		url.pathname = suffix;
		url.search = "";
		url.hash = "";
		return url.toString().replace(/\/+$/, "");
	} catch {
		return "";
	}
}

export function getDefaultControlPlaneBaseUrl(
	env: RuntimeEnv = import.meta.env,
) {
	const configured = env.VITE_DEFAULT_CONTROL_PLANE_BASE_URL?.trim();
	if (configured) {
		return adaptUrlToBrowserHost(configured);
	}

	if (!isDevRuntime(env)) {
		return "";
	}

	return adaptUrlToBrowserHost(
		deriveSiblingServiceUrl(getDefaultTenantApiBaseUrl(env), "3007"),
	);
}

export function getDefaultGatewayBaseUrl(env: RuntimeEnv = import.meta.env) {
	const configured = env.VITE_DEFAULT_GATEWAY_BASE_URL?.trim();
	if (configured) {
		return adaptUrlToBrowserHost(configured);
	}

	if (!isDevRuntime(env)) {
		return "/v1";
	}

	return adaptUrlToBrowserHost(
		deriveSiblingServiceUrl(getDefaultTenantApiBaseUrl(env), "3005", "/v1"),
	);
}

export function getConfiguredTenantManagementToken(
	env: RuntimeEnv = import.meta.env,
) {
	const configured = env.VITE_TENANT_MANAGEMENT_TOKEN?.trim();
	if (configured) {
		return configured;
	}

	if (!isDevRuntime(env)) {
		return "";
	}

	return env.VITE_DEV_TOKEN?.trim() ?? "";
}

export function getConfiguredControlPlaneToken(
	env: RuntimeEnv = import.meta.env,
) {
	const configured = env.VITE_CONTROL_PLANE_TOKEN?.trim();
	if (configured) {
		return configured;
	}

	if (!isDevRuntime(env)) {
		return "";
	}

	return env.VITE_DEV_CONTROL_PLANE_TOKEN?.trim() ?? "";
}

export function getConsoleSecretToken(env: RuntimeEnv = import.meta.env) {
	return env.VITE_CONSOLE_SECRET_TOKEN?.trim() ?? "";
}

export function getConsoleUsername(env: RuntimeEnv = import.meta.env) {
	return env.VITE_CONSOLE_USERNAME?.trim() ?? "";
}

export function getConsolePassword(env: RuntimeEnv = import.meta.env) {
	return env.VITE_CONSOLE_PASSWORD?.trim() ?? "";
}
