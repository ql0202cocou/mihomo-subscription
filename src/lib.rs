//! mihomo-subscription 库 crate。
//!
//! 暴露服务的各个构建块,使 `tests/` 下的集成测试能直接驱动它们。目标设计见 `docs/`。

pub mod app;
pub mod auth;
pub mod converter;
pub mod db;
pub mod error;
pub mod fetch;
pub mod generate;
pub mod global_nodes;
pub mod keyed_lock;
pub mod mask;
pub mod net;
pub mod profile_rule_sets;
pub mod profiles;
pub mod rate_limit;
pub mod rule_sets;
pub mod rulelib;
pub mod settings;
pub mod ssrf;
pub mod util;
pub mod yaml;
