use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use dashmap::DashMap;

use crate::config::parser::{parse_duration_secs, parse_size};
use crate::config::ServerConfig;

pub struct CachedEntry {
    pub body: Bytes,
    pub created_at: Instant,
    pub ttl: Duration,
    pub byte_size: usize,
    pub headers: Option<hyper::HeaderMap>,
}

pub struct ResponseCache {
    store: DashMap<String, Arc<CachedEntry>>,
    max_bytes: usize,
    current_bytes: AtomicUsize,
    default_ttl: Duration,
    revalidate_after: Duration,
    max_entry_bytes: usize,
    enabled: bool,
    mutation: std::sync::Mutex<()>,
}

impl ResponseCache {
    pub fn from_config(config: &ServerConfig) -> Self {
        let cache_cfg = config.cache.as_ref();
        let enabled = cache_cfg.map(|c| c.enabled).unwrap_or(true);
        let max_bytes = cache_cfg
            .and_then(|c| parse_size(&c.max_size))
            .unwrap_or(128 * 1024 * 1024) as usize;
        let ttl_secs = cache_cfg
            .and_then(|c| parse_duration_secs(&c.ttl))
            .unwrap_or(300);
        let max_entry_bytes = cache_cfg
            .and_then(|c| parse_size(&c.max_file_size))
            .unwrap_or(10 * 1024 * 1024) as usize;

        ResponseCache {
            store: DashMap::new(),
            max_bytes,
            current_bytes: AtomicUsize::new(0),
            default_ttl: Duration::from_secs(ttl_secs),
            revalidate_after: Duration::from_secs(
                cache_cfg
                    .and_then(|c| parse_duration_secs(&c.revalidate_after))
                    .unwrap_or(0)
                    .min(ttl_secs),
            ),
            max_entry_bytes,
            enabled,
            mutation: std::sync::Mutex::new(()),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn can_cache(&self, size: u64) -> bool {
        self.enabled && size > 0 && size <= self.max_entry_bytes.min(self.max_bytes) as u64
    }

    pub fn revalidate_after(&self) -> Duration {
        if self.enabled {
            self.revalidate_after
        } else {
            Duration::ZERO
        }
    }

    /// Fresh reads take no mutation lock. The mutex serialises eviction
    /// accounting only; holding it here funnelled every concurrent cache read in
    /// the process through one lock, which defeated the sharded map underneath.
    pub fn get(&self, key: &str) -> Option<Arc<CachedEntry>> {
        if !self.enabled {
            return None;
        }
        let entry = self.store.get(key)?;
        if entry.created_at.elapsed() > entry.ttl {
            drop(entry);
            let _guard = self.mutation.lock().unwrap_or_else(|e| e.into_inner());
            // Another request may have refreshed this key after the lookup.
            // Removal and accounting must also serialize with insert/eviction.
            if let Some((_, old)) = self
                .store
                .remove_if(key, |_, entry| entry.created_at.elapsed() > entry.ttl)
            {
                self.current_bytes
                    .fetch_sub(old.byte_size, Ordering::Relaxed);
            }
            return None;
        }
        Some(Arc::clone(&*entry))
    }

    pub fn insert(&self, key: String, entry: CachedEntry) {
        if !self.can_cache(entry.body.len() as u64) || entry.byte_size > self.max_bytes {
            return;
        }
        let _guard = self.mutation.lock().unwrap_or_else(|e| e.into_inner());
        let size = entry.byte_size;
        let current = self.current_bytes.load(Ordering::Relaxed);
        if current + size > self.max_bytes {
            self.evict(size);
        }
        let entry = Arc::new(entry);
        if let Some(old) = self.store.insert(key, entry) {
            self.current_bytes
                .fetch_sub(old.byte_size, Ordering::Relaxed);
        }
        self.current_bytes.fetch_add(size, Ordering::Relaxed);
    }

    fn evict(&self, needed: usize) {
        let mut freed = 0usize;
        let mut to_remove: Vec<String> = Vec::new();
        // Expire stale entries first
        for kv in self.store.iter() {
            if kv.created_at.elapsed() > kv.ttl {
                to_remove.push(kv.key().clone());
                freed += kv.byte_size;
            }
            if freed >= needed {
                break;
            }
        }
        // If still not enough, evict oldest by created_at
        if freed < needed {
            let mut candidates: Vec<(String, Instant)> = self
                .store
                .iter()
                .filter(|kv| !to_remove.contains(kv.key()))
                .map(|kv| (kv.key().clone(), kv.created_at))
                .collect();
            candidates.sort_by_key(|(_, t)| *t);
            for (key, _) in candidates {
                if let Some(kv) = self.store.get(&key) {
                    freed += kv.byte_size;
                    to_remove.push(key);
                }
                if freed >= needed {
                    break;
                }
            }
        }
        for key in to_remove {
            if let Some((_, old)) = self.store.remove(&key) {
                self.current_bytes
                    .fetch_sub(old.byte_size, Ordering::Relaxed);
            }
        }
    }

    pub fn invalidate(&self, key: &str) {
        let _guard = self.mutation.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, old)) = self.store.remove(key) {
            self.current_bytes
                .fetch_sub(old.byte_size, Ordering::Relaxed);
        }
    }

    pub fn default_ttl(&self) -> Duration {
        self.default_ttl
    }

    pub fn stats(&self) -> (usize, usize, usize) {
        (
            self.store.len(),
            self.current_bytes.load(Ordering::Relaxed),
            self.max_bytes,
        )
    }
}

impl CachedEntry {
    pub fn new(body: Bytes, ttl: Duration) -> Self {
        let byte_size = body.len();
        CachedEntry {
            body,
            created_at: Instant::now(),
            ttl,
            byte_size,
            headers: None,
        }
    }

    pub fn with_headers(mut self, headers: hyper::HeaderMap, key_length: usize) -> Self {
        // Include the additional response metadata in the same eviction budget.
        self.byte_size += key_length
            + std::mem::size_of::<Self>()
            + headers
                .iter()
                .map(|(k, v)| k.as_str().len() + v.as_bytes().len() + 64)
                .sum::<usize>();
        self.headers = Some(headers);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_expiry_and_replacement_keep_byte_accounting_consistent() {
        let cache = Arc::new(ResponseCache::from_config(&crate::default_config()));
        std::thread::scope(|scope| {
            for worker in 0..8 {
                let cache = cache.clone();
                scope.spawn(move || {
                    for i in 0..500 {
                        let ttl = if (i + worker) % 2 == 0 {
                            Duration::ZERO
                        } else {
                            Duration::from_secs(60)
                        };
                        cache.insert(
                            "shared".into(),
                            CachedEntry::new(Bytes::from(vec![0; 1024 + worker]), ttl),
                        );
                        let _ = cache.get("shared");
                    }
                });
            }
        });
        let actual: usize = cache.store.iter().map(|entry| entry.byte_size).sum();
        assert_eq!(cache.stats().1, actual);
        assert!(actual <= cache.stats().2);
        cache.invalidate("shared");
        assert_eq!(cache.stats().1, 0);
    }
}
