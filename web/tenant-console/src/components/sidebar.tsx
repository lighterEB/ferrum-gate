import { Link, useRouterState } from "@tanstack/react-router";
import {
	BellRingIcon,
	BlocksIcon,
	HouseIcon,
	KeyRoundIcon,
	ShieldEllipsisIcon,
	UsersRoundIcon,
} from "lucide-react";
import { useTranslation } from "react-i18next";

import { Badge } from "@/components/ui/badge";
import { consoleNavItems } from "@/lib/navigation";
import { cn } from "@/lib/utils";
import { useSession } from "@/session/store";

type SidebarProps = {
	mobile?: boolean;
	onNavigate?: () => void;
};

const NAV_ICONS = {
	"/dashboard": HouseIcon,
	"/accounts": UsersRoundIcon,
	"/api-keys": KeyRoundIcon,
	"/routing": BlocksIcon,
	"/alerts": BellRingIcon,
	"/audit": ShieldEllipsisIcon,
} as const;

function endpointHost(url: string | null) {
	if (!url) {
		return null;
	}

	try {
		return new URL(url).host;
	} catch {
		return url;
	}
}

export function Sidebar({ mobile = false, onNavigate }: SidebarProps) {
	const { t } = useTranslation();
	const session = useSession();
	const pathname = useRouterState({
		select: (state) => state.location.pathname,
	});

	return (
		<aside
			className={cn(
				"flex h-full flex-col border-r border-border/70 bg-sidebar/90 backdrop-blur-xl",
				mobile
					? "w-[min(82vw,20rem)] px-4 py-5"
					: "hidden w-60 shrink-0 px-4 py-5 lg:flex",
			)}
		>
			<div className="space-y-4">
				<div className="space-y-2 border-b border-border/60 pb-4">
					<div className="inline-flex items-center gap-2 rounded-full border border-cyan-400/25 bg-cyan-400/10 px-3 py-1 text-xs font-medium tracking-[0.18em] text-cyan-100 uppercase">
						FerrumGate
					</div>
					<div>
						<p className="text-sm font-semibold text-foreground">
							{t("app.title")}
						</p>
						<p className="mt-1 text-sm leading-6 text-muted-foreground">
							{t("app.subtitle")}
						</p>
					</div>
				</div>

				<nav className="space-y-1">
					{consoleNavItems.map((item) => {
						const Icon = NAV_ICONS[item.to];
						const active = pathname === item.to;

						return (
							<Link
								key={item.to}
								to={item.to}
								onClick={onNavigate}
								className={cn(
									"flex items-center gap-3 rounded-lg px-3 py-2.5 text-sm transition-colors",
									active
										? "bg-cyan-400/14 text-foreground"
										: "text-muted-foreground hover:bg-muted/60 hover:text-foreground",
								)}
							>
								<Icon className="size-4" />
								<span>{t(item.key)}</span>
							</Link>
						);
					})}
				</nav>
			</div>

			<div className="mt-auto space-y-3 border-t border-border/60 pt-4">
				<div className="rounded-lg border border-border/70 bg-background/70 px-3 py-3">
					<p className="text-xs font-medium tracking-[0.14em] uppercase text-muted-foreground">
						{t("app.chrome.endpoint")}
					</p>
					<p className="mt-2 text-sm text-foreground">
						{endpointHost(session.gatewayBaseUrl) ??
							endpointHost(session.baseUrl) ??
							t("dashboard.connections.unavailable")}
					</p>
				</div>
				<div className="flex items-center justify-between text-xs text-muted-foreground">
					<span>tenant-console</span>
					<Badge variant="outline">
						{import.meta.env.DEV ? "dev" : "prod"}
					</Badge>
				</div>
			</div>
		</aside>
	);
}
