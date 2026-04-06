import { useSyncExternalStore } from "react";

const listeners = new Set<() => void>();

let sidebarOpen = false;

function emitChange() {
	for (const listener of listeners) {
		listener();
	}
}

export function openSidebar() {
	sidebarOpen = true;
	emitChange();
}

export function closeSidebar() {
	sidebarOpen = false;
	emitChange();
}

export function toggleSidebar() {
	sidebarOpen = !sidebarOpen;
	emitChange();
}

export function useSidebarOpen() {
	const open = useSyncExternalStore(
		(listener) => {
			listeners.add(listener);
			return () => listeners.delete(listener);
		},
		() => sidebarOpen,
		() => sidebarOpen,
	);

	return {
		open,
		openSidebar,
		closeSidebar,
		toggleSidebar,
	};
}
