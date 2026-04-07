FROM oven/bun:1.2.18 AS frontend-builder

WORKDIR /app

COPY package.json bun.lock ./
COPY web/tenant-console/package.json web/tenant-console/package.json
RUN bun install --frozen-lockfile

COPY . .
WORKDIR /app/web/tenant-console
RUN bun run build

FROM nginx:1.27-alpine

RUN apk add --no-cache apache2-utils

COPY docker/nginx/backend.conf.template /etc/nginx/templates/default.conf.template
COPY docker/nginx/entrypoint/40-basic-auth.sh /docker-entrypoint.d/40-basic-auth.sh
COPY --from=frontend-builder /app/web/tenant-console/dist /usr/share/nginx/html
