import { useTranslation } from "react-i18next";

import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/components/ui/select";

function LanguageSwitcher() {
	const { i18n, t } = useTranslation();

	return (
		<div className="flex items-center gap-2">
			<span className="text-sm text-muted-foreground">
				{t("language.label")}
			</span>
			<Select
				value={i18n.resolvedLanguage ?? undefined}
				onValueChange={(value) => {
					if (value) {
						void i18n.changeLanguage(value);
					}
				}}
			>
				<SelectTrigger size="sm" className="min-w-28">
					<SelectValue />
				</SelectTrigger>
				<SelectContent align="end">
					<SelectItem value="zh-CN">{t("language.zh-CN")}</SelectItem>
					<SelectItem value="en-US">{t("language.en-US")}</SelectItem>
				</SelectContent>
			</Select>
		</div>
	);
}

export { LanguageSwitcher };
