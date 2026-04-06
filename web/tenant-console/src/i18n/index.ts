import i18n from "i18next";
import { initReactI18next } from "react-i18next";

import enUS from "@/i18n/locales/en-US";
import zhCN from "@/i18n/locales/zh-CN";

export const LANGUAGE_STORAGE_KEY = "fg.tenant.locale";

const resources = {
	"zh-CN": {
		translation: zhCN,
	},
	"en-US": {
		translation: enUS,
	},
} as const;

function detectLanguage() {
	if (typeof window === "undefined") {
		return "zh-CN";
	}

	const storedLanguage = window.localStorage.getItem(LANGUAGE_STORAGE_KEY);
	if (storedLanguage === "zh-CN" || storedLanguage === "en-US") {
		return storedLanguage;
	}

	return "zh-CN";
}

if (!i18n.isInitialized) {
	void i18n.use(initReactI18next).init({
		resources,
		lng: detectLanguage(),
		fallbackLng: "zh-CN",
		supportedLngs: ["zh-CN", "en-US"],
		defaultNS: "translation",
		interpolation: {
			escapeValue: false,
		},
	});

	i18n.on("languageChanged", (language) => {
		if (typeof window === "undefined") {
			return;
		}

		window.localStorage.setItem(LANGUAGE_STORAGE_KEY, language);
	});
}

export default i18n;
