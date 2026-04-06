import { useNavigate } from "@tanstack/react-router";
import {
	LockKeyholeIcon,
	ServerIcon,
	ShieldCheckIcon,
	UserRoundIcon,
} from "lucide-react";
import { useState } from "react";
import { useForm } from "react-hook-form";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import {
	Card,
	CardContent,
	CardDescription,
	CardHeader,
} from "@/components/ui/card";
import {
	Form,
	FormControl,
	FormField,
	FormItem,
	FormLabel,
	FormMessage,
} from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import {
	getControlPlaneApiErrorKey,
	listProviderAccounts,
} from "@/lib/control-plane-api";
import {
	getConfiguredControlPlaneToken,
	getConfiguredTenantManagementToken,
	getConsolePassword,
	getConsoleSecretToken,
	getConsoleUsername,
	getDefaultControlPlaneBaseUrl,
	getDefaultGatewayBaseUrl,
	getDefaultTenantApiBaseUrl,
} from "@/lib/env";
import { getGatewayHealth } from "@/lib/gateway-api";
import {
	getTenantApiErrorKey,
	getTenantMe,
	sanitizeTenantApiBaseUrl,
} from "@/lib/tenant-api";
import { connectSession } from "@/session/store";

type LoginMode = "token" | "password";

type ConnectValues = {
	secretToken: string;
	username: string;
	password: string;
};

function hostLabel(value: string, fallback: string) {
	if (!value) {
		return fallback;
	}

	try {
		return new URL(value).host;
	} catch {
		return fallback;
	}
}

export function ConnectPage() {
	const { t } = useTranslation();
	const navigate = useNavigate({ from: "/connect" });
	const tenantApiBaseUrl = sanitizeTenantApiBaseUrl(
		getDefaultTenantApiBaseUrl(),
	);
	const controlPlaneBaseUrl = sanitizeTenantApiBaseUrl(
		getDefaultControlPlaneBaseUrl(),
	);
	const gatewayBaseUrl = sanitizeTenantApiBaseUrl(getDefaultGatewayBaseUrl());
	const tenantManagementToken = getConfiguredTenantManagementToken();
	const controlPlaneToken = getConfiguredControlPlaneToken();
	const consoleSecretToken = getConsoleSecretToken();
	const consoleUsername = getConsoleUsername();
	const consolePassword = getConsolePassword();
	const hasSecretTokenLogin = Boolean(consoleSecretToken);
	const hasPasswordLogin = Boolean(consoleUsername && consolePassword);
	const [loginMode, setLoginMode] = useState<LoginMode>(
		hasSecretTokenLogin ? "token" : "password",
	);
	const form = useForm<ConnectValues>({
		defaultValues: {
			secretToken: "",
			username: "",
			password: "",
		},
	});
	const environmentReady = Boolean(
		tenantApiBaseUrl &&
			controlPlaneBaseUrl &&
			gatewayBaseUrl &&
			tenantManagementToken &&
			controlPlaneToken &&
			(hasSecretTokenLogin || hasPasswordLogin),
	);

	return (
		<div className="relative flex min-h-screen items-center justify-center overflow-hidden bg-[radial-gradient(circle_at_top,_rgba(34,211,238,0.14),_transparent_32%),linear-gradient(180deg,rgba(3,7,18,1)_0%,rgba(7,11,22,1)_100%)] px-4 py-10">
			<div className="absolute inset-0 bg-[linear-gradient(90deg,transparent_0%,rgba(15,23,42,0.28)_50%,transparent_100%)] opacity-40" />
			<Card className="relative w-full max-w-xl border-border/70 bg-card/92 shadow-[0_40px_120px_-60px_rgba(6,182,212,0.45)] backdrop-blur-xl">
				<CardHeader className="space-y-4 border-b border-border/70">
					<div className="inline-flex items-center gap-2 rounded-full border border-cyan-400/20 bg-cyan-400/10 px-3 py-1 text-xs font-medium tracking-[0.18em] text-cyan-100 uppercase">
						FerrumGate
					</div>
					<div className="space-y-2">
						<h1 className="text-3xl font-semibold tracking-tight">
							{t("connect.title")}
						</h1>
						<CardDescription className="text-sm leading-6">
							{t("connect.description")}
						</CardDescription>
					</div>
				</CardHeader>
				<CardContent className="space-y-6">
					<div className="rounded-lg border border-border/70 bg-background/70 px-4 py-4 text-sm text-muted-foreground">
						<div className="inline-flex items-center gap-2 font-medium text-foreground">
							<ShieldCheckIcon className="size-4" />
							<span>{t("connect.hero.noticeTitle")}</span>
						</div>
						<p className="mt-2 leading-6">{t("connect.hero.noticeBody")}</p>
					</div>

					{!environmentReady ? (
						<div className="rounded-lg border border-destructive/20 bg-destructive/10 px-4 py-3 text-sm text-destructive">
							{t("connect.errors.misconfigured")}
						</div>
					) : null}

					<div className="grid grid-cols-2 gap-2 rounded-lg border border-border/70 bg-background/60 p-1.5">
						{hasSecretTokenLogin ? (
							<Button
								variant={loginMode === "token" ? "default" : "ghost"}
								onClick={() => {
									setLoginMode("token");
									form.clearErrors();
								}}
							>
								{t("connect.modes.token")}
							</Button>
						) : null}
						{hasPasswordLogin ? (
							<Button
								variant={loginMode === "password" ? "default" : "ghost"}
								onClick={() => {
									setLoginMode("password");
									form.clearErrors();
								}}
							>
								{t("connect.modes.password")}
							</Button>
						) : null}
					</div>

					<Form {...form}>
						<form
							className="space-y-5"
							onSubmit={form.handleSubmit(async (values) => {
								form.clearErrors();

								if (!environmentReady) {
									toast.error(t("connect.errors.misconfigured"));
									return;
								}

								if (loginMode === "token") {
									const candidate = values.secretToken.trim();

									if (!candidate) {
										form.setError("secretToken", {
											message: t("connect.validation.secretTokenRequired"),
										});
										return;
									}

									if (!consoleSecretToken || candidate !== consoleSecretToken) {
										toast.error(t("connect.errors.invalidConsoleCredentials"));
										return;
									}
								} else {
									const username = values.username.trim();
									const password = values.password.trim();

									if (!username) {
										form.setError("username", {
											message: t("connect.validation.usernameRequired"),
										});
									}
									if (!password) {
										form.setError("password", {
											message: t("connect.validation.passwordRequired"),
										});
									}
									if (!username || !password) {
										return;
									}

									if (
										!consoleUsername ||
										!consolePassword ||
										username !== consoleUsername ||
										password !== consolePassword
									) {
										toast.error(t("connect.errors.invalidConsoleCredentials"));
										return;
									}
								}

								try {
									await Promise.all([
										getTenantMe(tenantApiBaseUrl, tenantManagementToken),
										listProviderAccounts(
											controlPlaneBaseUrl,
											controlPlaneToken,
										),
									]);

									connectSession({
										baseUrl: tenantApiBaseUrl,
										token: tenantManagementToken,
										controlPlaneBaseUrl,
										controlPlaneToken,
										gatewayBaseUrl,
									});

									void getGatewayHealth(gatewayBaseUrl).catch(() => {
										toast.error(t("connect.toast.gatewayWarning"));
									});
									toast.success(t("connect.toast.success"));
									await navigate({ to: "/dashboard" });
								} catch (error) {
									toast.error(
										error instanceof Error &&
											error.name === "ControlPlaneApiError"
											? t(getControlPlaneApiErrorKey(error))
											: t(getTenantApiErrorKey(error)),
									);
								}
							})}
						>
							{loginMode === "token" ? (
								<FormField
									control={form.control}
									name="secretToken"
									render={({ field }) => (
										<FormItem>
											<FormLabel>
												{t("connect.fields.secretToken.label")}
											</FormLabel>
											<FormControl>
												<Input
													type="password"
													placeholder={t(
														"connect.fields.secretToken.placeholder",
													)}
													{...field}
												/>
											</FormControl>
											<FormMessage />
										</FormItem>
									)}
								/>
							) : (
								<div className="grid gap-5">
									<FormField
										control={form.control}
										name="username"
										render={({ field }) => (
											<FormItem>
												<FormLabel>
													{t("connect.fields.username.label")}
												</FormLabel>
												<FormControl>
													<Input
														placeholder={t(
															"connect.fields.username.placeholder",
														)}
														autoComplete="username"
														{...field}
													/>
												</FormControl>
												<FormMessage />
											</FormItem>
										)}
									/>
									<FormField
										control={form.control}
										name="password"
										render={({ field }) => (
											<FormItem>
												<FormLabel>
													{t("connect.fields.password.label")}
												</FormLabel>
												<FormControl>
													<Input
														type="password"
														placeholder={t(
															"connect.fields.password.placeholder",
														)}
														autoComplete="current-password"
														{...field}
													/>
												</FormControl>
												<FormMessage />
											</FormItem>
										)}
									/>
								</div>
							)}

							<div className="grid gap-3 rounded-lg border border-border/70 bg-background/70 p-4 text-sm">
								<div className="inline-flex items-center gap-2 font-medium text-foreground">
									<UserRoundIcon className="size-4" />
									<span>{t("connect.environment.title")}</span>
								</div>
								<div className="grid gap-2 text-muted-foreground">
									<p>
										{t("connect.environment.tenant")}:{" "}
										{hostLabel(
											tenantApiBaseUrl,
											t("connect.hero.endpointFallback"),
										)}
									</p>
									<p>
										{t("connect.environment.controlPlane")}:{" "}
										{hostLabel(
											controlPlaneBaseUrl,
											t("connect.hero.endpointFallback"),
										)}
									</p>
									<p>
										{t("connect.environment.gateway")}:{" "}
										{hostLabel(
											gatewayBaseUrl,
											t("connect.hero.endpointFallback"),
										)}
									</p>
								</div>
							</div>

							<div className="flex items-center justify-between gap-4">
								<div className="inline-flex items-center gap-2 text-sm text-muted-foreground">
									<ServerIcon className="size-4" />
									<span>{t("connect.login.helper")}</span>
								</div>
								<Button type="submit" disabled={form.formState.isSubmitting}>
									<LockKeyholeIcon className="size-4" />
									{form.formState.isSubmitting
										? t("common.connecting")
										: t("common.connect")}
								</Button>
							</div>
						</form>
					</Form>
				</CardContent>
			</Card>
		</div>
	);
}
