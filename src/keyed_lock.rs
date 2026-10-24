//! 按 key 的互斥锁,是 single-flight 合并的基础设施。
//!
//! 本身只提供互斥:同 key 的并发 future 逐个串行执行,不共享结果;真正把并发公开请求
//! 合并为一次机场刷新,靠调用方在锁内重查缓存实现(见 `generate.rs::serve_or_refresh`
//! 与 `docs/architecture.md`)。入口是 [`KeyedLock::run`]:取锁、执行、归还一步到位,
//! 无等待者的条目在结束后即从锁表移除,使锁表随使用回收而非只增不减(清理思路参照
//! `src/rate_limit.rs` 的机会性清理)。

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use tokio::sync::Mutex as AsyncMutex;

#[derive(Clone, Default)]
pub struct KeyedLock {
    locks: Arc<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
}

struct EntryGuard {
    locks: Arc<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
    key: String,
    lock: Arc<AsyncMutex<()>>,
}

impl Drop for EntryGuard {
    fn drop(&mut self) {
        let mut map = self.locks.lock().unwrap();
        if Arc::strong_count(&self.lock) <= 2
            && map
                .get(&self.key)
                .is_some_and(|cur| Arc::ptr_eq(cur, &self.lock))
        {
            map.remove(&self.key);
        }
    }
}

impl KeyedLock {
    pub fn new() -> Self {
        Self::default()
    }

    /// 在 `key` 的锁内运行 `fut`,使同一 key 同一时刻只有一个刷新在跑。
    /// 取锁 → 执行 → 归还的次序由本方法保证,调用方无需关心锁表回收。
    pub async fn run<T>(&self, key: &str, fut: impl Future<Output = T>) -> T {
        let entry = self.entry_for(key);
        let result = {
            // 内层作用域保证异步锁 guard 先于 entry 释放，包括 future 被取消时。
            let _guard = entry.lock.lock().await;
            fut.await
        };
        drop(entry);
        result
    }

    /// 取 `key` 对应的异步锁,首次使用时创建。
    fn entry_for(&self, key: &str) -> EntryGuard {
        let mut map = self.locks.lock().unwrap();
        let lock = map.entry(key.to_string()).or_default().clone();
        EntryGuard {
            locks: Arc::clone(&self.locks),
            key: key.to_string(),
            lock,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn len(sf: &KeyedLock) -> usize {
        sf.locks.lock().unwrap().len()
    }

    #[tokio::test]
    async fn run_executes_and_cleans_up() {
        let sf = KeyedLock::new();
        let out = sf.run("k", async { 42 }).await;
        assert_eq!(out, 42);
        assert_eq!(len(&sf), 0, "run 结束后无等待者的条目被移除");
    }

    #[tokio::test]
    async fn entry_guard_removes_entry_without_waiters() {
        let sf = KeyedLock::new();
        let entry = sf.entry_for("k");
        {
            let _guard = entry.lock.lock().await;
        }
        drop(entry);
        assert_eq!(len(&sf), 0, "无等待者的条目在刷新结束后被移除");
    }

    #[tokio::test]
    async fn entry_guard_keeps_entry_with_waiters() {
        let sf = KeyedLock::new();
        let entry = sf.entry_for("k");
        let waiter = sf.entry_for("k");
        {
            let _guard = entry.lock.lock().await;
        }
        drop(entry);
        assert_eq!(len(&sf), 1, "仍有持有者时条目保留");
        drop(waiter);
        assert_eq!(len(&sf), 0, "最后的持有者结束后条目被移除");
    }

    #[tokio::test]
    async fn same_key_shares_lock_until_released() {
        let sf = KeyedLock::new();
        let a = sf.entry_for("k");
        let b = sf.entry_for("k");
        assert!(Arc::ptr_eq(&a.lock, &b.lock), "同 key 合并到同一把锁");
        let guard = a.lock.lock().await;
        assert!(b.lock.try_lock().is_err(), "锁被持有期间并发合并语义不变");
        drop(guard);
        drop(a);
        drop(b);
        assert_eq!(len(&sf), 0);
    }

    #[tokio::test]
    async fn abort_releases_lock_and_cleans_up_entry() {
        let sf = KeyedLock::new();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn({
            let sf = sf.clone();
            async move {
                sf.run("k", async move {
                    started_tx.send(()).unwrap();
                    std::future::pending::<()>().await;
                })
                .await;
            }
        });
        started_rx.await.unwrap();
        assert_eq!(len(&sf), 1);

        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(len(&sf), 0, "取消后无等待者的条目被移除");
        let out =
            tokio::time::timeout(std::time::Duration::from_secs(1), sf.run("k", async { 42 }))
                .await
                .expect("取消后的锁仍可获取");
        assert_eq!(out, 42);
    }
}
