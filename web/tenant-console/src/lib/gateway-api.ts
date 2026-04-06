export class GatewayApiError extends Error {
	status: number | null;
	code: string;

	constructor(code: string, message: string, status: number | null = null) {
		super(message);
		this.name = "GatewayApiError";
		this.code = code;
		this.status = status;
	}
}

function sanitizeGatewayBaseUrl(baseUrl: string) {
	return baseUrl.trim().replace(/\/+$/, "");
}

function healthUrl(baseUrl: string) {
	const sanitized = sanitizeGatewayBaseUrl(baseUrl);

	if (sanitized.endsWith("/v1")) {
		return `${sanitized.slice(0, -3)}/health`;
	}

	return `${sanitized}/health`;
}

export async function getGatewayHealth(baseUrl: string) {
	let response: Response;

	try {
		response = await fetch(healthUrl(baseUrl));
	} catch (error) {
		throw new GatewayApiError(
			"network",
			error instanceof Error ? error.message : "network error",
		);
	}

	if (!response.ok) {
		throw new GatewayApiError(
			`http_${response.status}`,
			response.statusText || "request failed",
			response.status,
		);
	}

	return response.json() as Promise<{ status: string }>;
}
