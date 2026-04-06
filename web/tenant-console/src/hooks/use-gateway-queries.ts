import { useQuery } from "@tanstack/react-query";

import { getGatewayHealth } from "@/lib/gateway-api";

export function useGatewayHealth(gatewayBaseUrl: string | null | undefined) {
	return useQuery({
		queryKey: ["gateway-health", gatewayBaseUrl],
		enabled: Boolean(gatewayBaseUrl),
		queryFn: async () => getGatewayHealth(gatewayBaseUrl ?? ""),
	});
}
