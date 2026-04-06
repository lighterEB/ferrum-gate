import { useQueryClient } from "@tanstack/react-query";

const WORKSPACE_QUERY_KEYS = [
	["tenant-me"],
	["tenant-models"],
	["tenant-api-keys"],
	["provider-accounts"],
	["alerts"],
	["audit-events"],
	["routing-overview"],
	["gateway-health"],
] as const;

export function useInvalidateWorkspace() {
	const queryClient = useQueryClient();

	return async function invalidateWorkspace() {
		await Promise.all(
			WORKSPACE_QUERY_KEYS.map((queryKey) =>
				queryClient.invalidateQueries({ queryKey }),
			),
		);
	};
}
