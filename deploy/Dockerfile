FROM nginxinc/nginx-unprivileged:1.27-alpine

COPY deploy/nginx.conf /etc/nginx/nginx.conf
COPY dist/ /usr/share/nginx/html/

# Pre-compress hashed assets so nginx serves them via `gzip_static on` with zero CPU cost.
USER root
RUN find /usr/share/nginx/html -type f \( -name '*.wasm' -o -name '*.js' -o -name '*.html' \) -exec gzip -k -9 {} + \
    && chown -R 101:101 /usr/share/nginx/html
USER 101

EXPOSE 8080
