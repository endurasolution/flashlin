use tracing::error;

pub fn log_error(request_id: &str, path: &str, message: &str) {
    error!(
        request_id = request_id,
        path = path,
        error = message,
        "server error"
    );
}
