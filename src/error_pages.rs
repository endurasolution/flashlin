/// Pre-built, styled HTML error pages for FlashLin.
///
/// Each function returns a complete HTML document.  The pages reference
/// `/logo.png` which is served from the public directory.

const STYLES: &str = r#"
*{box-sizing:border-box;margin:0;padding:0}
html,body{height:100%}
body{
  font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;
  background:#0b0d14;
  color:#e2e8f0;
  display:flex;
  flex-direction:column;
  align-items:center;
  justify-content:center;
  min-height:100vh;
  padding:2rem;
}
.card{
  text-align:center;
  max-width:520px;
  width:100%;
  padding:3rem 2.5rem 2.5rem;
  background:#141824;
  border:1px solid #1e2740;
  border-radius:16px;
  box-shadow:0 24px 64px rgba(0,0,0,.5);
}
.code{
  font-size:6rem;
  font-weight:900;
  letter-spacing:-4px;
  line-height:1;
  background:var(--accent);
  -webkit-background-clip:text;
  -webkit-text-fill-color:transparent;
  margin-bottom:.5rem;
}
.title{
  font-size:1.35rem;
  font-weight:700;
  color:#f1f5f9;
  margin-bottom:.75rem;
}
.desc{
  color:#64748b;
  line-height:1.7;
  font-size:.95rem;
  margin-bottom:2rem;
}
.actions{display:flex;gap:.75rem;justify-content:center;flex-wrap:wrap}
.btn{
  display:inline-flex;
  align-items:center;
  gap:.4rem;
  padding:.6rem 1.4rem;
  border-radius:8px;
  font-size:.875rem;
  font-weight:600;
  text-decoration:none;
  transition:opacity .15s;
  cursor:pointer;
  border:none;
}
.btn:hover{opacity:.85}
.btn-primary{background:#4f46e5;color:#fff}
.btn-ghost{background:transparent;color:#94a3b8;border:1px solid #1e2740}
.footer{
  margin-top:2.5rem;
  display:flex;
  flex-direction:column;
  align-items:center;
  gap:.6rem;
}
.footer img{
  height:28px;
  width:auto;
  opacity:.7;
  filter:brightness(1.2);
}
.footer-text{
  font-size:.7rem;
  color:#334155;
  letter-spacing:1px;
  text-transform:uppercase;
}
.divider{
  width:48px;
  height:1px;
  background:#1e2740;
  margin:1.5rem auto;
}
"#;

fn page(
    code: u16,
    accent: &str,
    title: &str,
    description: &str,
    server_name: &str,
    extra_actions: &str,
) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{code} {title}</title>
<style>:root{{--accent:{accent}}}{styles}</style>
</head>
<body>
<div class="card">
  <div class="code">{code}</div>
  <div class="title">{title}</div>
  <p class="desc">{description}</p>
  <div class="actions">
    <a href="/" class="btn btn-primary">&#8592; Back to Home</a>
    {extra_actions}
  </div>
  <div class="divider"></div>
  <div class="footer">
    <img src="/logo.png" alt="{server_name}" onerror="this.style.display='none'">
    <span class="footer-text">{server_name}</span>
  </div>
</div>
</body>
</html>"#,
        code = code,
        title = title,
        accent = accent,
        description = description,
        styles = STYLES,
        server_name = server_name,
        extra_actions = extra_actions,
    )
}

// ─── FlashLin-native 111 overload page ───────────────────────────────────────

/// 111 Server Overloaded — FlashLin's own internal overload signal.
///
/// This is NOT a standard HTTP code. FlashLin returns it when the active
/// connection count hits `max_connections`.  The page auto-retries after a
/// countdown and shows a pulsing overload indicator.
pub fn page_111(server_name: &str, active: usize, max: usize) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>111 Server Overloaded — {server_name}</title>
<style>
*{{box-sizing:border-box;margin:0;padding:0}}
html,body{{height:100%}}
body{{
  font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;
  background:#0b0d14;
  color:#e2e8f0;
  display:flex;
  flex-direction:column;
  align-items:center;
  justify-content:center;
  min-height:100vh;
  padding:2rem;
}}
.card{{
  text-align:center;
  max-width:540px;
  width:100%;
  padding:3rem 2.5rem 2.5rem;
  background:#141824;
  border:1px solid #2d1a1a;
  border-radius:16px;
  box-shadow:0 24px 64px rgba(0,0,0,.6),0 0 0 1px rgba(239,68,68,.08);
}}
/* Pulsing ring around the icon */
.ring{{
  width:80px;
  height:80px;
  border-radius:50%;
  border:2px solid rgba(239,68,68,.3);
  display:flex;
  align-items:center;
  justify-content:center;
  margin:0 auto 1.25rem;
  position:relative;
  animation:pulse-ring 2s ease infinite;
}}
.ring::before{{
  content:'';
  position:absolute;
  inset:-8px;
  border-radius:50%;
  border:1px solid rgba(239,68,68,.15);
  animation:pulse-ring 2s ease infinite .4s;
}}
.ring::after{{
  content:'';
  position:absolute;
  inset:-16px;
  border-radius:50%;
  border:1px solid rgba(239,68,68,.07);
  animation:pulse-ring 2s ease infinite .8s;
}}
@keyframes pulse-ring{{
  0%,100%{{opacity:1;transform:scale(1)}}
  50%{{opacity:.5;transform:scale(1.04)}}
}}
.icon{{font-size:2rem;line-height:1}}
.code{{
  font-size:5.5rem;
  font-weight:900;
  letter-spacing:-4px;
  line-height:1;
  background:linear-gradient(135deg,#ef4444,#f97316,#fbbf24);
  -webkit-background-clip:text;
  -webkit-text-fill-color:transparent;
  margin-bottom:.4rem;
}}
.badge{{
  display:inline-block;
  background:rgba(239,68,68,.12);
  border:1px solid rgba(239,68,68,.25);
  color:#f87171;
  font-size:.7rem;
  font-weight:700;
  letter-spacing:2px;
  text-transform:uppercase;
  padding:.25rem .75rem;
  border-radius:999px;
  margin-bottom:1rem;
}}
.title{{font-size:1.35rem;font-weight:700;color:#f1f5f9;margin-bottom:.75rem}}
.desc{{color:#64748b;line-height:1.7;font-size:.92rem;margin-bottom:1.5rem}}
.load-bar-wrap{{
  background:#0f1420;
  border-radius:999px;
  height:6px;
  overflow:hidden;
  margin-bottom:.4rem;
}}
.load-bar{{
  height:100%;
  border-radius:999px;
  background:linear-gradient(90deg,#ef4444,#f97316);
  width:{pct}%;
  transition:width .5s ease;
}}
.load-label{{
  font-size:.72rem;
  color:#475569;
  margin-bottom:1.75rem;
  display:flex;
  justify-content:space-between;
}}
.load-label span{{color:#94a3b8;font-weight:600}}
.actions{{display:flex;gap:.75rem;justify-content:center;flex-wrap:wrap;margin-bottom:1.5rem}}
.btn{{
  display:inline-flex;align-items:center;gap:.4rem;
  padding:.6rem 1.4rem;border-radius:8px;font-size:.875rem;font-weight:600;
  text-decoration:none;transition:opacity .15s;cursor:pointer;border:none;
}}
.btn:hover{{opacity:.85}}
.btn-danger{{background:#b91c1c;color:#fff}}
.btn-ghost{{background:transparent;color:#94a3b8;border:1px solid #1e2740}}
.retry-bar-wrap{{background:#0f1420;border-radius:999px;height:3px;overflow:hidden;margin-bottom:.35rem}}
.retry-bar{{height:100%;border-radius:999px;background:#3b82f6;width:100%;animation:countdown {retry}s linear forwards}}
@keyframes countdown{{from{{width:100%}}to{{width:0%}}}}
.retry-text{{font-size:.72rem;color:#334155;margin-bottom:1.5rem}}
.divider{{width:48px;height:1px;background:#1e2740;margin:0 auto 1.25rem}}
.footer{{display:flex;flex-direction:column;align-items:center;gap:.5rem}}
.footer img{{height:26px;width:auto;opacity:.6;filter:brightness(1.1)}}
.footer-text{{font-size:.7rem;color:#1e293b;letter-spacing:1px;text-transform:uppercase}}
</style>
</head>
<body>
<div class="card">

  <div class="ring"><span class="icon">⚡</span></div>

  <div class="code">111</div>
  <div class="badge">FlashLin · Server Overloaded</div>

  <div class="title">Server Overloaded</div>
  <p class="desc">
    {server_name} is currently handling the maximum number of connections.<br>
    Your request was not dropped — please retry in a moment.
  </p>

  <!-- Connection load bar -->
  <div class="load-bar-wrap"><div class="load-bar"></div></div>
  <div class="load-label">
    <span>Active connections</span>
    <span>{active} / {max}</span>
  </div>

  <div class="actions">
    <button onclick="location.reload()" class="btn btn-danger">&#8635; Retry Now</button>
    <a href="/" class="btn btn-ghost">&#8592; Home</a>
  </div>

  <!-- Auto-retry countdown -->
  <div class="retry-bar-wrap"><div class="retry-bar"></div></div>
  <p class="retry-text">Auto-retrying in {retry} seconds…</p>

  <div class="divider"></div>
  <div class="footer">
    <img src="/logo.png" alt="{server_name}" onerror="this.style.display='none'">
    <span class="footer-text">{server_name}</span>
  </div>

</div>
<script>
// Auto-reload when countdown ends
setTimeout(()=>location.reload(), {retry_ms});
</script>
</body>
</html>"#,
        server_name = server_name,
        active = active,
        max = max,
        pct = ((active as f64 / max.max(1) as f64) * 100.0).min(100.0) as u32,
        retry = 10,
        retry_ms = 10_000,
    )
}

// ─── Public page builders ─────────────────────────────────────────────────────

/// 404 Not Found
pub fn page_404(server_name: &str) -> String {
    page(
        404,
        "linear-gradient(135deg,#f59e0b,#ef4444)",
        "Not Found",
        "The page you are looking for doesn't exist or has been moved.<br>\
         Double-check the URL and try again.",
        server_name,
        r#"<button onclick="history.back()" class="btn btn-ghost">Go Back</button>"#,
    )
}

/// 403 Forbidden
pub fn page_403(server_name: &str) -> String {
    page(
        403,
        "linear-gradient(135deg,#f97316,#dc2626)",
        "Forbidden",
        "You don't have permission to access this resource.<br>\
         If you believe this is an error, please contact the site administrator.",
        server_name,
        r#"<button onclick="history.back()" class="btn btn-ghost">Go Back</button>"#,
    )
}

/// 500 Internal Server Error
pub fn page_500(server_name: &str) -> String {
    page(
        500,
        "linear-gradient(135deg,#8b5cf6,#ec4899)",
        "Internal Server Error",
        "Something went wrong on our end.<br>\
         The server encountered an unexpected condition. Please try again later.",
        server_name,
        r#"<a href="/" class="btn btn-ghost">Reload Page</a>"#,
    )
}

/// 502 Bad Gateway
pub fn page_502(server_name: &str) -> String {
    page(
        502,
        "linear-gradient(135deg,#6366f1,#8b5cf6)",
        "Bad Gateway",
        "The upstream server returned an invalid or unexpected response.<br>\
         The backend service may be down or misconfigured.",
        server_name,
        r#"<a href="javascript:location.reload()" class="btn btn-ghost">Try Again</a>"#,
    )
}

/// 503 Service Unavailable
pub fn page_503(server_name: &str) -> String {
    page(
        503,
        "linear-gradient(135deg,#0ea5e9,#6366f1)",
        "Service Unavailable",
        "The server is temporarily unavailable — this may be due to maintenance<br>\
         or high traffic. Please try again in a few moments.",
        server_name,
        r#"<a href="javascript:location.reload()" class="btn btn-ghost">Try Again</a>"#,
    )
}

/// 504 Gateway Timeout
pub fn page_504(server_name: &str) -> String {
    page(
        504,
        "linear-gradient(135deg,#14b8a6,#0ea5e9)",
        "Gateway Timeout",
        "The upstream server did not respond in time.<br>\
         It may be overloaded. Please try again shortly.",
        server_name,
        r#"<a href="javascript:location.reload()" class="btn btn-ghost">Retry</a>"#,
    )
}

/// 429 Too Many Requests
pub fn page_429(server_name: &str) -> String {
    page(
        429,
        "linear-gradient(135deg,#f59e0b,#f97316)",
        "Too Many Requests",
        "You have sent too many requests in a short period of time.<br>\
         Please slow down and try again in a minute.",
        server_name,
        r#"<a href="javascript:location.reload()" class="btn btn-ghost">Try Again</a>"#,
    )
}

/// 413 Payload Too Large
pub fn page_413(server_name: &str) -> String {
    page(
        413,
        "linear-gradient(135deg,#f97316,#f59e0b)",
        "Payload Too Large",
        "The request body exceeds the maximum allowed size.<br>\
         Please reduce the size of your request and try again.",
        server_name,
        r#"<button onclick="history.back()" class="btn btn-ghost">Go Back</button>"#,
    )
}

/// 405 Method Not Allowed
pub fn page_405(server_name: &str) -> String {
    page(
        405,
        "linear-gradient(135deg,#64748b,#475569)",
        "Method Not Allowed",
        "The HTTP method used is not allowed for this resource.<br>\
         Please check your request and try a different method.",
        server_name,
        r#"<button onclick="history.back()" class="btn btn-ghost">Go Back</button>"#,
    )
}
