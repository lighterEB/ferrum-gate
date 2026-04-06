import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import App from "@/app";
import "@/i18n";
import "@/index.css";
import "@/lib/theme";
import { hydrateSession } from "@/session/store";

hydrateSession();

const rootElement = document.getElementById("root");

if (!rootElement) {
	throw new Error("Root element #root not found");
}

createRoot(rootElement).render(
	<StrictMode>
		<App />
	</StrictMode>,
);
