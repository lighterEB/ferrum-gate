import { useNavigate, useRouterState } from "@tanstack/react-router";
import {
	AlignJustifyIcon,
	LogOutIcon,
	MonitorCogIcon,
	MoonStarIcon,
	SunMediumIcon,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { LanguageSwitcher } from "@/components/language-switcher";
import { Button } from "@/components/ui/button";
import { getRouteTitle } from "@/lib/navigation";
import { useSidebarOpen } from "@/lib/sidebar-state";
import { useTheme } from "@/lib/theme";
import { disconnectSession } from "@/session/store";

export function TopBar() {
	const { t } = useTranslation();
	const navigate = useNavigate();
	const pathname = useRouterState({
		select: (state) => state.location.pathname,
	});
	const { toggleSidebar } = useSidebarOpen();
	const { theme, toggleTheme } = useTheme();

	return (
		<header className="flex items-center justify-between gap-4 border-b border-border/70 bg-background/80 px-4 py-4 backdrop-blur-xl sm:px-6">
			<div className="flex items-center gap-3">
				<Button
					variant="ghost"
					size="icon"
					className="lg:hidden"
					onClick={toggleSidebar}
					aria-label="Open navigation"
				>
					<AlignJustifyIcon className="size-4" />
				</Button>
				<div className="space-y-1">
					<div className="inline-flex items-center gap-2 text-xs font-medium tracking-[0.14em] text-muted-foreground uppercase">
						<MonitorCogIcon className="size-3.5" />
						FerrumGate
					</div>
					<p className="text-sm font-medium text-foreground">
						{getRouteTitle(pathname, t)}
					</p>
				</div>
			</div>

			<div className="flex items-center gap-2">
				<Button
					variant="outline"
					size="icon"
					onClick={toggleTheme}
					aria-label={t("theme.toggle")}
				>
					{theme === "dark" ? (
						<SunMediumIcon className="size-4" />
					) : (
						<MoonStarIcon className="size-4" />
					)}
				</Button>
				<LanguageSwitcher />
				<Button
					variant="outline"
					size="sm"
					onClick={() => {
						disconnectSession();
						toast.success(t("apiKeys.toast.disconnected"));
						void navigate({ to: "/connect" });
					}}
				>
					<LogOutIcon className="size-4" />
					{t("common.disconnect")}
				</Button>
			</div>
		</header>
	);
}
