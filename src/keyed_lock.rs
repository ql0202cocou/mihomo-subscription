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

impl KeyedLock {
    pub fn new() -> Self {
        Self::default()
    }

    /// 在 `key` 的锁内运行 `fut`,使同一 key 同一时刻只有一个刷新在跑。
    /// 取锁 → 执行 → 归还的次序由本方法保证,调用方无需关心锁表回收。
    pub async fn run<T>(&self, key: &str, fut: impl Future<Output = T>) -> T {
        let lock = self.lock_for(key);
        let result = {
            let _guard = lock.lock().await;
            fut.await
        };
        self.release(key, &lock);
        result
    }

    /// 取 `key` 对应的异步锁,首次使用时创建。
    fn lock_for(&self, key: &str) -> Arc<AsyncMutex<()>> {
        let mut map = self.locks.lock().unwrap();
        map.entry(key.to_string()).or_default().clone()
    }

    /// 刷新结束后归还 `key` 的锁。调用方须先 drop 掉 guard(否则新请求可能拿到一把新锁,
    /// 与仍在跑的旧刷新并发)。此时若没有其他持有者(等待者),强计数为 2(锁表 + 调用方),
    /// 条目可安全移除;有等待者则保留,由最后结束的持有者移除。
    fn release(&self, key: &str, lock: &Arc<AsyncMutex<()>>) {
        let mut map = self.locks.lock().unwrap();
        if Arc::strong_count(lock) <= 2 && map.get(key).is_some_and(|cur| Arc::ptr_eq(cur, lock)) {
            map.remove(key);
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
    async fn release_removes_entry_without_waiters() {
        let sf = KeyedLock::new();
        let lock = sf.lock_for("k");
        {
            let _guard = lock.lock().await;
        }
        sf.release("k", &lock);
        assert_eq!(len(&sf), 0, "无等待者的条目在刷新结束后被移除");
    }

    #[tokio::test]
    async fn release_keeps_entry_with_waiters() {
        let sf = KeyedLock::new();
        let lock = sf.lock_for("k");
        let waiter = sf.lock_for("k"); // 等待者持有的克隆
        {
            let _guard = lock.lock().await;
        }
        sf.release("k", &lock); // 锁表 + lock + waiter = 3 → 保留
        assert_eq!(len(&sf), 1, "仍有持有者时条目保留");
        drop(lock);
        sf.release("k", &waiter); // 锁表 + waiter = 2 → 移除
        assert_eq!(len(&sf), 0, "最后的持有者结束后条目被移除");
    }

    #[tokio::test]
    async fn same_key_shares_lock_until_released() {
        let sf = KeyedLock::new();
        let a = sf.lock_for("k");
        let b = sf.lock_for("k");
        assert!(Arc::ptr_eq(&a, &b), "同 key 合并到同一把锁");
        let guard = a.lock().await;
        assert!(b.try_lock().is_err(), "锁被持有期间并发合并语义不变");
        drop(guard);
        drop(a);
        sf.release("k", &b);
        assert_eq!(len(&sf), 0);
    }
}
