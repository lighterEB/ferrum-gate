const enUS = {
	app: {
		title: "FerrumGate Console",
		subtitle:
			"Operate the account pool, API keys, routing, and downstream access from one workspace.",
		chrome: {
			endpoint: "Endpoint",
			workspace: "Workspace",
		},
		navigation: {
			connect: "Connect",
			dashboard: "Dashboard",
			accounts: "Accounts",
			apiKeys: "API Keys",
			routing: "Routing",
			alerts: "Alerts",
			audit: "Audit",
			docs: "Docs",
		},
	},
	language: {
		label: "Language",
		"zh-CN": "中文",
		"en-US": "English",
	},
	theme: {
		dark: "Dark",
		light: "Light",
		toggle: "Toggle theme",
	},
	common: {
		connect: "Connect",
		connecting: "Connecting...",
		cancel: "Cancel",
		close: "Close",
		create: "Create",
		rotate: "Rotate",
		revoke: "Revoke",
		refresh: "Refresh",
		retry: "Retry",
		disconnect: "Disconnect",
		search: "Search",
		copy: "Copy",
		copied: "Copied",
		never: "Never",
		loading: "Loading...",
		saving: "Saving...",
		unknown: "Unknown",
	},
	status: {
		all: "All statuses",
		active: "Active",
		revoked: "Revoked",
		disabled: "Disabled",
		draining: "Draining",
		cooling: "Cooling",
		quotaExhausted: "Quota Exhausted",
		invalidCredentials: "Invalid Credentials",
		healthy: "Healthy",
		unhealthy: "Unhealthy",
		online: "Online",
		offline: "Offline",
	},
	connect: {
		title: "Connect FerrumGate Console",
		description:
			"Use a console credential to verify the environment and enter the workspace.",
		login: {
			title: "Console Sign In",
			helper:
				"The console connects to the tenant API, control plane, and gateway for you.",
		},
		modes: {
			token: "Secret Token",
			password: "Username / Password",
		},
		fields: {
			secretToken: {
				label: "Console Secret Token",
				placeholder: "Enter the console secret token",
			},
			username: {
				label: "Username",
				placeholder: "Enter the username",
			},
			password: {
				label: "Password",
				placeholder: "Enter the password",
			},
		},
		environment: {
			title: "Environment Summary",
			tenant: "Tenant API",
			controlPlane: "Control Plane",
			gateway: "Gateway",
		},
		hero: {
			eyebrow: "Ops Console",
			body: "The sign-in surface validates the operator credential while backend targets stay fixed by deployment.",
			noticeTitle: "Sign-in method",
			noticeBody:
				"Use either a secret token or a username/password pair. The browser stores only the current session.",
			endpointFallback: "Not configured",
		},
		toast: {
			success: "Connection verified. The console is ready.",
			gatewayWarning:
				"The gateway is unreachable right now, but you can still enter the console.",
		},
		errors: {
			misconfigured:
				"The console environment is not configured yet. Check the frontend deployment variables.",
			invalidConsoleCredentials:
				"Sign-in failed. Check the console secret token or username/password.",
		},
		validation: {
			secretTokenRequired: "Console secret token is required.",
			usernameRequired: "Username is required.",
			passwordRequired: "Password is required.",
		},
	},
	dashboard: {
		title: "Dashboard",
		description: "Start with system health, then move into operations.",
		metrics: {
			accounts: "Accounts",
			healthy: "Healthy",
			exceptions: "Exceptions",
			apiKeys: "Active Keys",
			models: "Available Models",
			gateway: "Gateway",
		},
		hints: {
			accounts: "Provider accounts currently visible in the control plane.",
			healthy: "Accounts that remain active.",
			exceptions:
				"Accounts currently in cooling, disabled, invalid, or quota exhausted states.",
			apiKeys: "Tenant API keys that remain usable for downstream callers.",
			models: "Models exposed through the tenant API.",
			gatewayUp: "The gateway is online and ready for downstream access.",
			gatewayDown:
				"The gateway is offline and downstream validation will fail.",
		},
		connections: {
			title: "Connection Summary",
			tenant: "Tenant API",
			control: "Control Plane",
			gateway: "Gateway",
			unavailable: "Unavailable",
		},
		modelsSection: {
			title: "Available Models",
			count: "{{count}} model(s)",
			empty: "There are no available models right now.",
		},
		quickActions: {
			title: "Quick Actions",
			refresh: "Refresh Data",
			accounts: "Open Accounts",
			apiKeys: "Manage API Keys",
			docs: "Open Docs",
		},
	},
	accounts: {
		title: "Accounts",
		description:
			"Inspect account state, capabilities, quota snapshots, and operator actions from one table.",
		filterLabel: "Filter by status",
		empty: "There are no provider accounts yet.",
		capabilities: "Capabilities",
		noCapabilities: "No capability records yet.",
		quota: "Quota Snapshot",
		quotaMissing: "No quota snapshot yet",
		expand: "Expand Details",
		collapse: "Collapse Details",
		columns: {
			identity: "Email / Display",
			status: "Status",
			provider: "Provider",
			plan: "Plan",
			lastValidated: "Last Probe",
			expiresAt: "Expires At",
			actions: "Actions",
		},
		actions: {
			probe: "Probe",
			quota: "Quota",
			refresh: "Refresh",
			enable: "Enable",
			disable: "Disable",
			drain: "Drain",
		},
	},
	apiKeys: {
		title: "API Keys",
		description:
			"Create, rotate, and revoke tenant API keys. Plaintext secrets appear only once.",
		createTitle: "Issue a New Key",
		createDescription:
			"Use a readable label, then reveal a one-time secret for the downstream caller.",
		label: "Label",
		placeholder: "For example: OpenWebUI / Cherry Studio / Production",
		createSubmit: "Create and Reveal Secret",
		empty: "There are no tenant API keys yet.",
		columns: {
			label: "Label",
			prefix: "Prefix",
			status: "Status",
			createdAt: "Created At",
			lastUsedAt: "Last Used At",
			actions: "Actions",
		},
		toast: {
			created: "API key created.",
			rotated: "API key rotated.",
			revoked: "API key revoked.",
			copied: "Secret copied to the clipboard.",
			disconnected: "Local connection details were cleared.",
		},
		dialogs: {
			create: {
				submit: "Create and Reveal Secret",
			},
		},
		validation: {
			labelRequired: "Label is required.",
			labelTooLong: "Label must be 64 characters or fewer.",
		},
	},
	routing: {
		title: "Routing",
		description:
			"Models are derived automatically from account capabilities. Route groups and bindings are managed automatically by default.",
		summary:
			"Manual route-group and binding APIs remain available only for advanced overrides.",
		empty: "There are no route groups yet.",
		columns: {
			publicModel: "Public Model",
			provider: "Provider",
			upstreamModel: "Upstream Model",
			bindings: "Bound Accounts",
		},
	},
	alerts: {
		title: "Alerts",
		description: "Review current unhealthy events by resource.",
		empty: "There are no alert items right now.",
		columns: {
			kind: "Kind",
			severity: "Severity",
			resource: "Resource",
			message: "Message",
			time: "Time",
		},
	},
	audit: {
		title: "Audit",
		description: "Review the control-plane audit trail.",
		empty: "There are no audit events right now.",
		columns: {
			action: "Action",
			actor: "Actor",
			resource: "Resource",
			requestId: "Request ID",
			time: "Time",
		},
	},
	docs: {
		title: "Integration Docs",
		description:
			"Hand downstream apps the correct endpoint, auth pattern, model list, and working examples.",
		steps: {
			one: "1. Create or rotate an API key and copy the full fgk_ secret.",
			two: "2. Point downstream apps at the gateway base URL, not the tenant API or control plane.",
			three: "3. Use only model names from the published models list below.",
		},
		baseUrl: "Base URL",
		apiKey: "API Key",
		models: "Published Models",
		curlModels: "List Models",
		curlChat: "Chat Example",
		important:
			"Do not use the prefix as the API key. The full secret is available only during create or rotate.",
		liveTip:
			"If the gateway is offline, fix the service before handing these docs to downstream callers.",
		empty: "There are no publishable models right now.",
	},
	secretCard: {
		title: "One-time Secret",
		description:
			"This secret is shown only once. Copy it now and configure the caller immediately.",
		createLabel: "Create Result",
		rotateLabel: "Rotate Result",
	},
	errors: {
		network:
			"Unable to reach the target service. Check the URL, network, or CORS configuration.",
		unauthorized: "Authentication failed. Check the current token.",
		forbidden: "The current token does not have permission for this action.",
		notFound: "The requested resource was not found.",
		generic: "The request failed. Please try again.",
	},
} as const;

export default enUS;
