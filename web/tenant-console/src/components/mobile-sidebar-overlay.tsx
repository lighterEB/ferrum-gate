import { Sidebar } from "@/components/sidebar";
import { closeSidebar, useSidebarOpen } from "@/lib/sidebar-state";
import { cn } from "@/lib/utils";

export function MobileSidebarOverlay() {
	const { open } = useSidebarOpen();

	return (
		<div
			className={cn(
				"fixed inset-0 z-50 lg:hidden",
				open ? "pointer-events-auto" : "pointer-events-none",
			)}
		>
			<button
				type="button"
				aria-label="Close navigation"
				className={cn(
					"absolute inset-0 bg-black/60 transition-opacity",
					open ? "opacity-100" : "opacity-0",
				)}
				onClick={closeSidebar}
			/>
			<div
				className={cn(
					"absolute inset-y-0 left-0 transition-transform duration-200",
					open ? "translate-x-0" : "-translate-x-full",
				)}
			>
				<Sidebar mobile onNavigate={closeSidebar} />
			</div>
		</div>
	);
}
