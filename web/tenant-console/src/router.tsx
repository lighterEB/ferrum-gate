import {
	createMemoryHistory,
	createRootRoute,
	createRoute,
	createRouter,
	Outlet,
	type RouterHistory,
	redirect,
} from "@tanstack/react-router";

import { AppShell } from "@/components/app-shell";
import { AccountsPage } from "@/pages/accounts-page";
import { AlertsPage } from "@/pages/alerts-page";
import { ApiKeysPage } from "@/pages/api-keys-page";
import { AuditPage } from "@/pages/audit-page";
import { ConnectPage } from "@/pages/connect-page";
import { DashboardPage } from "@/pages/dashboard-page";
import { RoutingPage } from "@/pages/routing-page";
import { isConnected } from "@/session/store";

function RootLayout() {
	return (
		<AppShell>
			<Outlet />
		</AppShell>
	);
}

function requireConnected() {
	if (!isConnected()) {
		throw redirect({ to: "/connect" });
	}
}

const rootRoute = createRootRoute({
	component: RootLayout,
});

const indexRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/",
	beforeLoad: () => {
		throw redirect({ to: isConnected() ? "/dashboard" : "/connect" });
	},
});

const connectRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/connect",
	beforeLoad: () => {
		if (isConnected()) {
			throw redirect({ to: "/dashboard" });
		}
	},
	component: ConnectPage,
});

const dashboardRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/dashboard",
	beforeLoad: requireConnected,
	component: DashboardPage,
});

const accountsRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/accounts",
	beforeLoad: requireConnected,
	component: AccountsPage,
});

const apiKeysRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/api-keys",
	beforeLoad: requireConnected,
	component: ApiKeysPage,
});

const routingRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/routing",
	beforeLoad: requireConnected,
	component: RoutingPage,
});

const alertsRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/alerts",
	beforeLoad: requireConnected,
	component: AlertsPage,
});

const auditRoute = createRoute({
	getParentRoute: () => rootRoute,
	path: "/audit",
	beforeLoad: requireConnected,
	component: AuditPage,
});

const routeTree = rootRoute.addChildren([
	indexRoute,
	connectRoute,
	dashboardRoute,
	accountsRoute,
	apiKeysRoute,
	routingRoute,
	alertsRoute,
	auditRoute,
]);

export function createAppRouter(history?: RouterHistory) {
	return createRouter({
		routeTree,
		...(history ? { history } : {}),
	});
}

export const router = createAppRouter();

export { createMemoryHistory };

declare module "@tanstack/react-router" {
	interface Register {
		router: typeof router;
	}
}
