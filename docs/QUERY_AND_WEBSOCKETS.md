# QUERY and WebSocket support

A complete [example configuration](../examples/query-websocket.toml) is included.

## WebSockets: available through the reverse proxy

Flashlin forwards HTTP/1.1 WebSocket upgrades and tunnels traffic in both
directions. The application upstream performs the WebSocket handshake and handles
messages. Use `http://` or `https://` upstream URLs, not `ws://` URLs:

```toml
[[routes]]
path = "/socket"
proxy_pass = "http://127.0.0.1:3000"
methods = ["GET"]
php_enabled = false
```

Clients connect to `ws://your-host/socket`, or `wss://your-host/socket` when
Flashlin HTTPS is configured. HTTPS upstreams require the `tls`/`acme` build
feature. Configure application heartbeats below the socket idle deadlines.
HTTP/2 and HTTP/3 extended CONNECT WebSockets are not implemented. Upgrade requests are
never automatically replayed; the process permits up to 1,024 active tunnels.

## QUERY: application requests with content

[RFC 10008](https://www.rfc-editor.org/rfc/rfc10008.html) defines the case-sensitive
`QUERY` method for safe, idempotent operations with request content. It is distinct
from the `?key=value` portion of a URL. Flashlin transports QUERY; it does not
execute SQL, invent a search language or implement application query semantics.

Append a route targeting an application that understands QUERY:

```toml
[[routes]]
path = "/search"
proxy_pass = "http://127.0.0.1:3000"
methods = ["QUERY", "OPTIONS"]
php_enabled = false
```

```sh
curl -X QUERY http://127.0.0.1:8080/search \
  -H 'Content-Type: application/json' \
  --data '{"filter":{"status":"active"},"limit":20}'
```

The backend decides which formats and operations are supported. It should only
perform read-only, repeatable work for QUERY. Unsupported formats can produce
415; invalid query semantics can produce 422. Flashlin returns 400 for missing,
duplicate or malformed Content-Type, and 413 for bodies exceeding the configured
limit. Bodies remain buffered within `security.max_body_size` and `read_timeout`.

QUERY works over downstream HTTP/1.1, HTTP/2 and optional HTTP/3. The proxy preserves the method,
body bytes, escaped URI and end-to-end headers, including conditional/range request
headers and response Accept-Query, Location and Content-Location. It does not
follow redirects on behalf of the client. An upstream owns their interpretation.
Static files and directory listings do not implement QUERY and return 405 with
Allow; method-denied routes also report their configured Allow list. Method names
in `routes.methods` now match case-sensitively, so use uppercase standard names.

PHP routes retain REQUEST_METHOD=QUERY, content type and body length, including
empty content. PHP applications must read the raw request body (for example,
`php://input`) and implement QUERY themselves; do not depend on POST form parsing.

## Discovery, CORS, retry and caching

For proxy routes, OPTIONS is forwarded so the backend can return Allow,
Accept-Query and CORS headers. Include OPTIONS in an explicit route method list.
Accept-Query uses Structured Fields syntax; for example:

```http
Allow: QUERY, OPTIONS
Accept-Query: application/json
Access-Control-Allow-Origin: https://client.example
Access-Control-Allow-Methods: QUERY, OPTIONS
Access-Control-Allow-Headers: Content-Type
```

If an applicable `.flashweb` file defines CORS, Flashlin continues handling OPTIONS
locally instead. Configure `methods = ["QUERY", "OPTIONS"]` in its `[cors]` section,
the required request headers and a specific allowed origin. To advertise query
formats on locally handled OPTIONS, set an explicit `.flashweb` response header:

```toml
[[header]]
name = "Accept-Query"
value = "application/json"
```

Only advertise formats actually understood by the application. Flashlin does not
guess them. Cross-origin browser QUERY calls require preflight.

For `proxy_upstreams`, a fully buffered QUERY may fail over to up to three eligible
backends on 502/503/504, preserving the same body. Responses are not replayed after
successful headers/body streaming have begun. POST and WebSocket replay policy
is unchanged. Applications that mutate state must not expose those actions as QUERY.

Flashlin does **not** cache QUERY results in its static-file RAM cache. Different
bodies at the same URL go to the application independently. Any external cache
must account for request content and representation metadata; URL-only keys are
not safe for QUERY results. Upstream cache directives are forwarded, subject to
explicit administrator `.flashweb` overrides.

## Verification scope

The TCP tests cover HTTP/1.1 and HTTP/2 body forwarding, different bodies at the
same URL, chunked requests, preserved URI/response headers, OPTIONS discovery,
local CORS, body limits, media-type checks, case-sensitive method permissions,
static-file rejection and body-preserving failover. CGI metadata has a unit test;
an actual PHP application was not exercised. Existing WebSocket tunnel regression
coverage also runs. This is transport support, not a claim of a built-in query engine
or a complete RFC conformance certification.
