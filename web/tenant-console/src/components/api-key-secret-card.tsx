import { CheckIcon, CopyIcon, EyeIcon } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import {
	Card,
	CardAction,
	CardContent,
	CardDescription,
	CardHeader,
	CardTitle,
} from "@/components/ui/card";

type SecretCardProps = {
	label: string;
	secret: string;
	kind: "create" | "rotate";
	onDismiss: () => void;
};

function ApiKeySecretCard({ label, secret, kind, onDismiss }: SecretCardProps) {
	const { t } = useTranslation();
	const [copied, setCopied] = useState(false);

	return (
		<Card className="border-primary/15 bg-primary/5">
			<CardHeader className="border-b border-primary/10">
				<div className="flex items-start gap-3">
					<div className="mt-0.5 rounded-full bg-primary/10 p-2 text-primary">
						<EyeIcon className="size-4" />
					</div>
					<div className="flex-1 space-y-1">
						<CardTitle>{t("secretCard.title")}</CardTitle>
						<CardDescription>{t("secretCard.description")}</CardDescription>
					</div>
				</div>
				<CardAction>
					<Button variant="outline" size="sm" onClick={onDismiss}>
						{t("common.close")}
					</Button>
				</CardAction>
			</CardHeader>
			<CardContent className="space-y-4">
				<div className="grid gap-3 sm:grid-cols-[1fr_auto] sm:items-end">
					<div className="space-y-1">
						<p className="text-xs font-medium uppercase tracking-[0.16em] text-muted-foreground">
							{kind === "create"
								? t("secretCard.createLabel")
								: t("secretCard.rotateLabel")}
						</p>
						<p className="text-sm font-medium text-foreground">{label}</p>
					</div>
					<Button
						variant="secondary"
						size="sm"
						onClick={async () => {
							await navigator.clipboard.writeText(secret);
							setCopied(true);
							toast.success(t("apiKeys.toast.copied"));
						}}
					>
						{copied ? (
							<CheckIcon className="size-4" />
						) : (
							<CopyIcon className="size-4" />
						)}
						{copied ? t("common.copied") : t("common.copy")}
					</Button>
				</div>

				<div className="overflow-x-auto rounded-xl border border-border bg-background px-4 py-3 font-mono text-sm text-foreground">
					{secret}
				</div>
			</CardContent>
		</Card>
	);
}

export { ApiKeySecretCard };
