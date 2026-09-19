use tracing::info;

pub struct AccessEntry<'a> {
    pub request_id: &'a str,
    pub remote_ip: &'a str,
    pub method: &'a str,
    pub path: &'a str,
    pub status: u16,
    pub bytes_sent: usize,
    pub duration_ms: u128,
    pub user_agent: &'a str,
}

impl<'a> AccessEntry<'a> {
    pub fn log(&self) {
        info!(
            request_id = self.request_id,
            ip = self.remote_ip,
            method = self.method,
            path = self.path,
            status = self.status,
            bytes = self.bytes_sent,
            ms = self.duration_ms,
            ua = self.user_agent,
            "access"
        );
    }
}
