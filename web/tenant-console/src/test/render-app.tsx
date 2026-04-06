import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { render } from "@testing-library/react";

import { Toaster } from "@/components/ui/sonner";
import { createAppRouter, createMemoryHistory } from "@/router";

export function renderApp(initialPath = "/connect") {
	const queryClient = new QueryClient({
		defaultOptions: {
			queries: {
				retry: false,
				refetchOnWindowFocus: false,
			},
		},
	});

	const history = createMemoryHistory({
		initialEntries: [initialPath],
	});
	const router = createAppRouter(history);

	const view = render(
		<QueryClientProvider client={queryClient}>
			<RouterProvider router={router} />
			<Toaster position="top-right" richColors closeButton />
		</QueryClientProvider>,
	);

	return {
		...view,
		history,
		queryClient,
		router,
	};
}
