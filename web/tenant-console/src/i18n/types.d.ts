import "i18next";

import zhCN from "@/i18n/locales/zh-CN";

declare module "i18next" {
	interface CustomTypeOptions {
		defaultNS: "translation";
		resources: {
			translation: typeof zhCN;
		};
	}
}
