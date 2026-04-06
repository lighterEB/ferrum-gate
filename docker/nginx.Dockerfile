FROM nginx:1.27-alpine

COPY docker/nginx/backend.conf /etc/nginx/conf.d/default.conf

