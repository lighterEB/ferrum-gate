import { useRouterState } from "@tanstack/react-router";
import { type PropsWithChildren, useEffect } from "react";

import { MobileSidebarOverlay } from "@/components/mobile-sidebar-overlay";
import { Sidebar } from "@/components/sidebar";
import { TopBar } from "@/components/top-bar";
import { closeSidebar } from "@/lib/sidebar-state";

export function AppShell({ children }: PropsWithChildren) {
	const pathname = useRouterState({
		select: (state) => state.location.pathname,
	});
	const isConnectPage = pathname === "/connect";

	useEffect(() => {
		if (pathname) {
			closeSidebar();
		}
	}, [pathname]);

	if (isConnectPage) {
		return (
			<div className="min-h-screen bg-background text-foreground">
				<main>{children}</main>
			</div>
		);
	}

	return (
		<div className="flex h-screen bg-background text-foreground">
			<Sidebar />
			<MobileSidebarOverlay />
			<div className="flex min-w-0 flex-1 flex-col">
				<TopBar />
				<main className="flex-1 overflow-y-auto px-4 py-5 sm:px-6 sm:py-6 lg:px-8">
					{children}
				</main>
			</div>
		</div>
	);
}
