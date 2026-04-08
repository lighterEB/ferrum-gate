FROM nginx:1.27-alpine

COPY docker/nginx/backend.conf.template /etc/nginx/templates/default.conf.template
